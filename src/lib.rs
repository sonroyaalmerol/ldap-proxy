use concread::arcache::ARCache;
use hashbrown::HashSet;
use ldap3_proto::parse_ldap_filter_str;
use ldap3_proto::{LdapFilter, LdapSearchScope};
use openssl::ssl::SslConnector;
use redis::aio::ConnectionManager;
use serde::Deserialize;
use serde_with::DeserializeFromStr;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use url::Url;

pub mod proxy;

use crate::proxy::{CachedValue, SearchCacheKey};

const MEGABYTES: usize = 1048576;

#[derive(Clone)]
pub enum CacheBackend {
    Memory(ARCache<SearchCacheKey, CachedValue>),
    Redis(ConnectionManager),
}

pub struct AppState {
    pub tls_params: SslConnector,
    pub addrs: Vec<SocketAddr>,
    pub binddn_map: BTreeMap<String, DnConfig>,
    pub cache: CacheBackend,
    pub cache_ttl: Option<u64>,
    pub max_incoming_ber_size: Option<usize>,
    pub max_proxy_ber_size: Option<usize>,
    pub allow_all_bind_dns: bool,
    pub remote_ip_addr_info: AddrInfoSource,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DnConfig {
    #[serde(default)]
    pub allowed_queries: HashSet<(String, LdapSearchScope, LdapFilterWrapper)>,
}

#[derive(DeserializeFromStr, Debug, Clone, PartialEq, Eq, Hash)]
pub struct LdapFilterWrapper {
    pub inner: LdapFilter,
}

impl FromStr for LdapFilterWrapper {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_ldap_filter_str(s)
            .map(|inner| LdapFilterWrapper { inner })
            .map_err(|err| err.to_string())
    }
}

fn default_fallback_cache_bytes() -> usize {
    256 * MEGABYTES
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
pub enum AddrInfoSource {
    #[default]
    None,
    ProxyV2,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CacheConfig {
    Memory {
        #[serde(default = "default_fallback_cache_bytes")]
        size_bytes: usize,
    },
    Redis {
        url: String,
        #[serde(default)]
        ttl_seconds: Option<u64>,
        #[serde(default = "default_redis_key_prefix")]
        key_prefix: String,
    },
}

fn default_redis_key_prefix() -> String {
    "ldap_proxy:".to_string()
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig::Memory {
            size_bytes: default_fallback_cache_bytes(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub bind: SocketAddr,
    pub tls_key: PathBuf,
    pub tls_chain: PathBuf,

    #[serde(default)]
    pub cache: CacheConfig,

    // Deprecated: use cache.size_bytes instead
    #[serde(default = "default_fallback_cache_bytes")]
    pub fallback_cache_bytes: usize,

    pub ldap_ca: PathBuf,
    pub ldap_url: Url,

    #[serde(default)]
    pub remote_ip_addr_info: AddrInfoSource,

    pub max_incoming_ber_size: Option<usize>,
    pub max_proxy_ber_size: Option<usize>,

    #[serde(default)]
    pub allow_all_bind_dns: bool,

    #[serde(flatten)]
    pub binddn_map: BTreeMap<String, DnConfig>,
}