use crate::{AppState, CacheBackend, DnConfig, LdapFilterWrapper};
use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use ldap3_proto::control::LdapControl;
use ldap3_proto::proto::*;
use ldap3_proto::LdapCodec;
use openssl::ssl::{Ssl, SslConnector};
use redis::AsyncCommands;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, error, info, span, trace, warn, Level};

type CR = ReadHalf<SslStream<TcpStream>>;
type CW = WriteHalf<SslStream<TcpStream>>;

#[derive(Debug, Clone, Hash, PartialOrd, Ord, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchCacheKey {
    bind_dn: String,
    search: LdapSearchRequest,
    ctrl: Vec<LdapControl>,
}

impl SearchCacheKey {
    pub fn to_redis_key(&self, prefix: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        format!("{}{:x}", prefix, hasher.finish())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedValue {
    pub cached_at: std::time::SystemTime,
    pub entries: Vec<(LdapSearchResultEntry, Vec<LdapControl>)>,
    pub result: LdapResult,
    pub ctrl: Vec<LdapControl>,
}

impl CachedValue {
    pub fn size(&self) -> usize {
        std::mem::size_of::<Self>() + self.entries.iter().map(|(e, _)| e.size()).sum::<usize>()
    }
}

enum ClientState {
    Unbound,
    Authenticated {
        dn: String,
        config: DnConfig,
        client: BasicLdapClient,
    },
}

fn bind_operror(msgid: i32, msg: &str) -> LdapMsg {
    LdapMsg {
        msgid,
        op: LdapOp::BindResponse(LdapBindResponse {
            res: LdapResult {
                code: LdapResultCode::OperationsError,
                matcheddn: "".to_string(),
                message: msg.to_string(),
                referral: vec![],
            },
            saslcreds: None,
        }),
        ctrl: vec![],
    }
}

// Tiered cache structure for Redis backend
struct TieredCache {
    l1_cache: Arc<Mutex<HashMap<SearchCacheKey, CachedValue>>>,
    redis_conn: redis::aio::ConnectionManager,
    max_l1_size: usize,
}

impl TieredCache {
    fn new(
        redis_conn: redis::aio::ConnectionManager,
        max_l1_size: usize,
    ) -> Self {
        Self {
            l1_cache: Arc::new(Mutex::new(HashMap::new())),
            redis_conn,
            max_l1_size,
        }
    }

    async fn get(
        &self,
        key: &SearchCacheKey,
        redis_prefix: &str,
    ) -> Option<CachedValue> {
        // Check L1 cache first
        {
            let cache = self.l1_cache.lock().unwrap();
            if let Some(value) = cache.get(key) {
                trace!("L1 cache hit");
                return Some(value.clone());
            }
        }

        // L1 miss, check Redis (L2)
        let redis_key = key.to_redis_key(redis_prefix);
        let mut conn = self.redis_conn.clone();
        
        match conn.get::<_, Vec<u8>>(&redis_key).await {
            Ok(data) => match serde_json::from_slice::<CachedValue>(&data) {
                Ok(value) => {
                    trace!("L2 (Redis) cache hit, promoting to L1");
                    // Promote to L1 cache
                    {
                        let mut cache = self.l1_cache.lock().unwrap();
                        
                        // Simple eviction if over size
                        if cache.len() >= self.max_l1_size {
                            // Remove oldest entry (simple FIFO eviction)
                            if let Some(first_key) = cache.keys().next().cloned() {
                                cache.remove(&first_key);
                            }
                        }
                        
                        cache.insert(key.clone(), value.clone());
                    }
                    Some(value)
                }
                Err(e) => {
                    error!(?e, "Failed to deserialize cached value from Redis");
                    None
                }
            },
            Err(e) => {
                match e.kind() {
                    redis::ErrorKind::TypeError => {
                        trace!("Cache miss on both L1 and L2");
                    }
                    _ => {
                        debug!(?e, "Redis get failed");
                    }
                }
                None
            }
        }
    }

    async fn set(
        &self,
        key: SearchCacheKey,
        value: CachedValue,
        redis_prefix: &str,
        ttl: Option<u64>,
    ) {
        // Write to L1 cache immediately
        {
            let mut cache = self.l1_cache.lock().unwrap();
            
            // Simple eviction if over size
            if cache.len() >= self.max_l1_size {
                if let Some(first_key) = cache.keys().next().cloned() {
                    cache.remove(&first_key);
                }
            }
            
            cache.insert(key.clone(), value.clone());
        }

        // Write to Redis synchronously with timeout
        let redis_key = key.to_redis_key(redis_prefix);
        let mut conn = self.redis_conn.clone();
        
        let timeout = Duration::from_millis(100);
        let redis_write = async {
            match serde_json::to_vec(&value) {
                Ok(data) => {
                    let result = if let Some(ttl_seconds) = ttl {
                        conn.set_ex::<_, _, ()>(&redis_key, data, ttl_seconds).await
                    } else {
                        conn.set::<_, _, ()>(&redis_key, data).await
                    };
                    
                    if let Err(e) = result {
                        debug!(?e, "Redis write failed");
                    } else {
                        trace!("Redis write completed");
                    }
                }
                Err(e) => {
                    error!(?e, "Failed to serialize value for Redis");
                }
            }
        };

        // Wait for Redis write with timeout
        if tokio::time::timeout(timeout, redis_write).await.is_err() {
            warn!("Redis write timed out, continuing with L1 cache only");
        }
    }

    async fn set_if_changed(
        &self,
        key: SearchCacheKey,
        value: CachedValue,
        redis_prefix: &str,
        ttl: Option<u64>,
    ) {
        // Check if data has changed by comparing with existing cache
        let existing = self.get(&key, redis_prefix).await;
        
        let has_changed = match existing {
            Some(cached) => {
                // Compare the actual data (entries and result)
                // We ignore cached_at timestamp for comparison
                cached.entries != value.entries 
                    || cached.result.code != value.result.code
                    || cached.result.message != value.result.message
                    || cached.ctrl != value.ctrl
            }
            None => {
                // No existing cache, definitely changed
                true
            }
        };

        if has_changed {
            debug!("Cache data has changed, updating");
            self.set(key, value, redis_prefix, ttl).await;
        } else {
            debug!("Cache data unchanged, skipping Redis write");
            // Still update L1 to refresh the entry
            let mut cache = self.l1_cache.lock().unwrap();
            
            // Simple eviction if over size
            if cache.len() >= self.max_l1_size {
                if let Some(first_key) = cache.keys().next().cloned() {
                    cache.remove(&first_key);
                }
            }
            
            cache.insert(key, value);
        }
    }
}

async fn cache_get(
    cache: &CacheBackend,
    key: &SearchCacheKey,
    redis_prefix: &str,
    tiered_cache: &Option<Arc<TieredCache>>,
) -> Option<CachedValue> {
    match cache {
        CacheBackend::Memory(mem_cache) => {
            let mut cache_read = mem_cache.read();
            cache_read.get(key).cloned()
        }
        CacheBackend::Redis(_) => {
            if let Some(tc) = tiered_cache {
                tc.get(key, redis_prefix).await
            } else {
                None
            }
        }
    }
}

async fn cache_set(
    cache: &CacheBackend,
    key: SearchCacheKey,
    value: CachedValue,
    redis_prefix: &str,
    ttl: Option<u64>,
    tiered_cache: &Option<Arc<TieredCache>>,
) {
    match cache {
        CacheBackend::Memory(mem_cache) => {
            let mut cache_write = mem_cache.write();
            if let Some(cache_value_size) = NonZeroUsize::new(value.size()) {
                debug!("Updating memory cache with entry of size {}", cache_value_size);
                cache_write.insert_sized(key, value, cache_value_size);
            } else {
                error!("Invalid entry size, unable to add to memory cache");
            }
        }
        CacheBackend::Redis(_) => {
            if let Some(tc) = tiered_cache {
                tc.set(key, value, redis_prefix, ttl).await;
                debug!("Updated tiered cache (L1 + L2)");
            }
        }
    }
}

async fn cache_set_if_changed(
    cache: &CacheBackend,
    key: SearchCacheKey,
    value: CachedValue,
    redis_prefix: &str,
    ttl: Option<u64>,
    tiered_cache: &Option<Arc<TieredCache>>,
) {
    match cache {
        CacheBackend::Memory(mem_cache) => {
            let mut cache_write = mem_cache.write();
            if let Some(cache_value_size) = NonZeroUsize::new(value.size()) {
                debug!("Updating memory cache with entry of size {}", cache_value_size);
                cache_write.insert_sized(key, value, cache_value_size);
            } else {
                error!("Invalid entry size, unable to add to memory cache");
            }
        }
        CacheBackend::Redis(_) => {
            if let Some(tc) = tiered_cache {
                tc.set_if_changed(key, value, redis_prefix, ttl).await;
            }
        }
    }
}

async fn cache_try_quiesce(cache: &CacheBackend) {
    if let CacheBackend::Memory(mem_cache) = cache {
        mem_cache.try_quiesce();
    }
}

pub async fn client_process<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    mut r: FramedRead<R, LdapCodec>,
    mut w: FramedWrite<W, LdapCodec>,
    client_address: SocketAddr,
    reported_client_address: Option<SocketAddr>,
    app_state: Arc<AppState>,
) {
    if let Some(reported_client_address) = reported_client_address {
        info!(?reported_client_address, via = ?client_address, "new client");
    } else {
        info!(?client_address, "new client");
    };

    let mut state = ClientState::Unbound;
    let redis_prefix = "ldap_proxy:".to_string();

    // Initialize tiered cache if using Redis backend
    let tiered_cache = match &app_state.cache {
        CacheBackend::Redis(conn) => {
            // L1 cache size: 1000 entries (adjust as needed)
            Some(Arc::new(TieredCache::new(conn.clone(), 1000)))
        }
        _ => None,
    };

    while let Some(Ok(protomsg)) = r.next().await {
        let next_state = match (&mut state, protomsg) {
            (
                _,
                LdapMsg {
                    msgid,
                    op: LdapOp::BindRequest(lbr),
                    ctrl,
                },
            ) => {
                let span = span!(Level::INFO, "bind");
                let _enter = span.enter();

                trace!(?lbr);
                let config = match app_state.binddn_map.get(&lbr.dn) {
                    Some(dnconfig) => dnconfig.clone(),
                    None => {
                        if app_state.allow_all_bind_dns {
                            DnConfig::default()
                        } else {
                            let resp_msg = bind_operror(msgid, "unable to bind");
                            if w.send(resp_msg).await.is_err() {
                                error!("Unable to send response");
                                break;
                            }
                            continue;
                        }
                    }
                };

                let dn = lbr.dn.clone();

                let mut client = match BasicLdapClient::build(
                    &app_state.addrs,
                    &app_state.tls_params,
                    app_state.max_proxy_ber_size,
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        error!(?e, "A client build error has occurred.");
                        let resp_msg = bind_operror(msgid, "unable to bind");
                        if w.send(resp_msg).await.is_err() {
                            error!("Unable to send response");
                        }
                        break;
                    }
                };

                let valid = match client.bind(lbr, ctrl).await {
                    Ok((bind_resp, ctrl)) => {
                        let valid = bind_resp.res.code == LdapResultCode::Success;

                        let resp_msg = LdapMsg {
                            msgid,
                            op: LdapOp::BindResponse(bind_resp),
                            ctrl,
                        };
                        if w.send(resp_msg).await.is_err() {
                            error!("Unable to send response");
                            break;
                        }
                        valid
                    }
                    Err(e) => {
                        error!(?e, "A client bind error has occurred");
                        let resp_msg = bind_operror(msgid, "unable to bind");
                        if w.send(resp_msg).await.is_err() {
                            error!("Unable to send response");
                        }
                        break;
                    }
                };

                if valid {
                    info!("Successful bind for {}", dn);
                    Some(ClientState::Authenticated { dn, config, client })
                } else {
                    None
                }
            }
            (
                _,
                LdapMsg {
                    msgid: _,
                    op: LdapOp::UnbindRequest,
                    ctrl: _,
                },
            ) => {
                trace!("unbind");
                break;
            }

            (
                ClientState::Authenticated {
                    dn,
                    config,
                    ref mut client,
                },
                LdapMsg {
                    msgid,
                    op: LdapOp::SearchRequest(sr),
                    ctrl,
                },
            ) => {
                let span = span!(Level::INFO, "search");
                let _enter = span.enter();

                if config.allowed_queries.is_empty() {
                    debug!("All queries are allowed");
                } else {
                    let allow_key = (
                        sr.base.clone(),
                        sr.scope.clone(),
                        LdapFilterWrapper {
                            inner: sr.filter.clone(),
                        },
                    );

                    if config.allowed_queries.contains(&allow_key) {
                        debug!("Query is granted");
                    } else {
                        warn!(?allow_key, "Requested query is not allowed for {}", dn);
                        if w.send(LdapMsg {
                            msgid,
                            op: LdapOp::SearchResultDone(LdapResult {
                                code: LdapResultCode::Success,
                                matcheddn: "".to_string(),
                                message: "".to_string(),
                                referral: vec![],
                            }),
                            ctrl,
                        })
                        .await
                        .is_err()
                        {
                            error!("Unable to send response");
                        }
                        break;
                    }
                };

                let cache_key = SearchCacheKey {
                    bind_dn: dn.clone(),
                    search: sr.clone(),
                    ctrl: ctrl.clone(),
                };
                debug!(?cache_key);

                let (entries, result, ctrl) = match client.search(sr, ctrl).await {
                    Ok(data) => {
                        info!("Backend is reachable, updating fallback cache");
                        let (entries, result, ctrl) = data;
                        
                        let cache_value = CachedValue {
                            cached_at: std::time::SystemTime::now(),
                            entries: entries.clone(),
                            result: result.clone(),
                            ctrl: ctrl.clone(),
                        };
                        
                        cache_set_if_changed(
                            &app_state.cache,
                            cache_key.clone(),
                            cache_value,
                            &redis_prefix,
                            app_state.cache_ttl,
                            &tiered_cache,
                        )
                        .await;
                        
                        (entries, result, ctrl)
                    }
                    Err(e) => {
                        warn!(?e, "Backend is unreachable, attempting to use fallback cache");
                        
                        match cache_get(&app_state.cache, &cache_key, &redis_prefix, &tiered_cache).await {
                            Some(cached_value) => {
                                info!("Serving from fallback cache (cached at: {:?})", cached_value.cached_at);
                                (
                                    cached_value.entries.clone(),
                                    cached_value.result.clone(),
                                    cached_value.ctrl.clone(),
                                )
                            }
                            None => {
                                error!("Backend unreachable and no fallback data available");
                                let resp_msg = LdapMsg {
                                    msgid,
                                    op: LdapOp::SearchResultDone(LdapResult {
                                        code: LdapResultCode::Unavailable,
                                        matcheddn: "".to_string(),
                                        message: "Backend LDAP server unavailable and no cached data".to_string(),
                                        referral: vec![],
                                    }),
                                    ctrl: vec![],
                                };
                                if w.send(resp_msg).await.is_err() {
                                    error!("Unable to send response");
                                }
                                break;
                            }
                        }
                    }
                };

                for (entry, ctrl) in entries {
                    if w.send(LdapMsg {
                        msgid,
                        op: LdapOp::SearchResultEntry(entry),
                        ctrl,
                    })
                    .await
                    .is_err()
                    {
                        error!("Unable to send response");
                        break;
                    }
                }

                if w.send(LdapMsg {
                    msgid,
                    op: LdapOp::SearchResultDone(result),
                    ctrl,
                })
                .await
                .is_err()
                {
                    error!("Unable to send response");
                    break;
                }

                cache_try_quiesce(&app_state.cache).await;

                None
            }
            (
                ClientState::Authenticated {
                    dn,
                    config: _,
                    client: _,
                },
                LdapMsg {
                    msgid,
                    op: LdapOp::ExtendedRequest(ler),
                    ctrl: _,
                },
            ) => {
                let op = match ler.name.as_str() {
                    "1.3.6.1.4.1.4203.1.11.3" => LdapOp::ExtendedResponse(LdapExtendedResponse {
                        res: LdapResult {
                            code: LdapResultCode::Success,
                            matcheddn: "".to_string(),
                            message: "".to_string(),
                            referral: vec![],
                        },
                        name: None,
                        value: Some(Vec::from(dn.as_str())),
                    }),
                    _ => LdapOp::ExtendedResponse(LdapExtendedResponse {
                        res: LdapResult {
                            code: LdapResultCode::OperationsError,
                            matcheddn: "".to_string(),
                            message: "".to_string(),
                            referral: vec![],
                        },
                        name: None,
                        value: None,
                    }),
                };

                if w.send(LdapMsg {
                    msgid,
                    op,
                    ctrl: vec![],
                })
                .await
                .is_err()
                {
                    error!("Unable to send response");
                    break;
                }

                None
            }
            (_, msg) => {
                debug!(?msg);
                break;
            }
        };

        if let Some(next_state) = next_state {
            state = next_state;
        }
    }
    info!("Disconnect for {}", client_address);
}

#[derive(Debug, Clone)]
pub enum LdapError {
    TlsError,
    ConnectError,
    Transport,
    InvalidProtocolState,
}

pub struct BasicLdapClient {
    r: FramedRead<CR, LdapCodec>,
    w: FramedWrite<CW, LdapCodec>,
    msg_counter: i32,
}

impl BasicLdapClient {
    fn next_msgid(&mut self) -> i32 {
        self.msg_counter += 1;
        self.msg_counter
    }

    pub async fn build(
        addrs: &[SocketAddr],
        tls_connector: &SslConnector,
        max_ber_size: Option<usize>,
    ) -> Result<Self, LdapError> {
        let timeout = Duration::from_secs(5);

        let mut aiter = addrs.iter();

        let tcpstream = loop {
            if let Some(addr) = aiter.next() {
                let sleep = tokio::time::sleep(timeout);
                tokio::pin!(sleep);
                tokio::select! {
                    maybe_stream = TcpStream::connect(addr) => {
                        match maybe_stream {
                            Ok(t) => {
                                trace!(?addr, "connection established");
                                break t;
                            }
                            Err(e) => {
                                trace!(?addr, ?e, "error");
                                continue;
                            }
                        }
                    }
                    _ = &mut sleep => {
                        warn!(?addr, "timeout");
                        continue;
                    }
                }
            } else {
                return Err(LdapError::ConnectError);
            }
        };

        let mut tlsstream = Ssl::new(tls_connector.context())
            .and_then(|tls_obj| SslStream::new(tls_obj, tcpstream))
            .map_err(|e| {
                error!(?e, "openssl");
                LdapError::TlsError
            })?;

        SslStream::connect(Pin::new(&mut tlsstream))
            .await
            .map_err(|e| {
                error!(?e, "openssl");
                LdapError::TlsError
            })?;

        let (r, w) = tokio::io::split(tlsstream);

        let w = FramedWrite::new(w, LdapCodec::new(max_ber_size));
        let r = FramedRead::new(r, LdapCodec::new(max_ber_size));

        info!("Connected to remote ldap server");
        Ok(BasicLdapClient {
            r,
            w,
            msg_counter: 0,
        })
    }

    pub async fn bind(
        &mut self,
        lbr: LdapBindRequest,
        ctrl: Vec<LdapControl>,
    ) -> Result<(LdapBindResponse, Vec<LdapControl>), LdapError> {
        let ck_msgid = self.next_msgid();

        let msg = LdapMsg {
            msgid: ck_msgid,
            op: LdapOp::BindRequest(lbr),
            ctrl,
        };

        self.w.send(msg).await.map_err(|e| {
            error!(?e, "unable to transmit to ldap server");
            LdapError::Transport
        })?;

        match self.r.next().await {
            Some(Ok(LdapMsg {
                msgid,
                op: LdapOp::BindResponse(bind_resp),
                ctrl,
            })) => {
                if msgid == ck_msgid {
                    Ok((bind_resp, ctrl))
                } else {
                    error!("invalid msgid, sequence error.");
                    Err(LdapError::InvalidProtocolState)
                }
            }
            Some(Ok(msg)) => {
                trace!(?msg);
                Err(LdapError::InvalidProtocolState)
            }
            Some(Err(e)) => {
                error!(?e, "unable to receive from ldap server");
                Err(LdapError::Transport)
            }
            None => {
                error!("connection closed");
                Err(LdapError::Transport)
            }
        }
    }

    pub async fn search(
        &mut self,
        sr: LdapSearchRequest,
        ctrl: Vec<LdapControl>,
    ) -> Result<
        (
            Vec<(LdapSearchResultEntry, Vec<LdapControl>)>,
            LdapResult,
            Vec<LdapControl>,
        ),
        LdapError,
    > {
        let ck_msgid = self.next_msgid();

        let msg = LdapMsg {
            msgid: ck_msgid,
            op: LdapOp::SearchRequest(sr),
            ctrl,
        };

        self.w.send(msg).await.map_err(|e| {
            error!(?e, "unable to transmit to ldap server");
            LdapError::Transport
        })?;

        let mut entries = Vec::new();
        loop {
            match self.r.next().await {
                // This terminates the iteration of entries.
                Some(Ok(LdapMsg {
                    msgid,
                    op: LdapOp::SearchResultDone(search_res),
                    ctrl,
                })) => {
                    if msgid == ck_msgid {
                        break Ok((entries, search_res, ctrl));
                    } else {
                        error!("invalid msgid, sequence error.");
                        break Err(LdapError::InvalidProtocolState);
                    }
                }
                Some(Ok(LdapMsg {
                    msgid,
                    op: LdapOp::SearchResultEntry(search_entry),
                    ctrl,
                })) => {
                    if msgid == ck_msgid {
                        entries.push((search_entry, ctrl))
                    } else {
                        error!("invalid msgid, sequence error.");
                        break Err(LdapError::InvalidProtocolState);
                    }
                }
                Some(Ok(msg)) => {
                    trace!(?msg);
                    break Err(LdapError::InvalidProtocolState);
                }
                Some(Err(e)) => {
                    error!(?e, "unable to receive from ldap server");
                    break Err(LdapError::Transport);
                }
                None => {
                    error!("connection closed");
                    break Err(LdapError::Transport);
                }
            }
        }
    }
}