# Ldap Proxy

A fast, simple, fallback proxy for LDAP that provides high availability and allows limiting of DNs and their searches.

## Overview

ldap-proxy acts as a transparent LDAP proxy with automatic fallback capabilities. It continuously attempts to forward requests to the upstream LDAP server while maintaining a fallback cache. When the backend LDAP server becomes unavailable, ldap-proxy seamlessly serves cached data, ensuring your applications remain operational during outages.

## Features

- **Transparent Proxying**: All requests are forwarded to the upstream LDAP server when available
- **Automatic Fallback**: Seamlessly serves cached data when the backend is unreachable
- **No Cache Expiration**: Fallback data never expires until replaced with fresh data from the backend (memory cache) or TTL expires (Redis cache)
- **Flexible Cache Backends**: Choose between in-memory cache or Redis for distributed deployments
- **LDAP Firewall**: Filter which DNs can bind and what queries they may perform
- **High Performance**: Configurable cache with size limits (memory) or TTL (Redis)
- **TLS/LDAPS Support**: Secure connections for both client and upstream server

## Configuration

### Basic Configuration (Memory Cache)

```toml
# /data/config.toml for containers.
# /etc/ldap-proxy/config.toml for packaged versions.

bind = "127.0.0.1:3636"
tls_chain = "/tmp/chain.pem"
tls_key = "/tmp/key.pem"

# Cache configuration - Memory backend (default)
[cache]
type = "memory"
size_bytes = 268435456  # 256 MB (default)

# The max ber size of requests from clients
# max_incoming_ber_size = 8388608
# The max ber size of responses from the upstream ldap server
# max_proxy_ber_size = 8388608

# By default only DNs listed in the bind-maps may bind. All other
# DNs that do not have a bind-map entry may not proceed. Setting
# this allows all DNs to bind through the server. When this is
# true, if the DN has a bind-map it will filter the queries of that
# DN. If the DN does not have a bind map, it allows all queries.
#
# Another way to think of this is that setting this to "false"
# makes this an LDAP firewall. Setting this to "true" turns this
# into a transparent fallback proxy with optional query filtering.
#
# allow_all_bind_dns = false

ldap_ca = "/tmp/ldap-ca.pem"
ldap_url = "ldaps://idm.example.com"

# Optional: Configure source of client IP address information
# Options: "None" (default), "ProxyV2" (for HAProxy PROXY protocol v2)
# remote_ip_addr_info = "None"


# Bind Maps
#
# This allows you to configure which DNs can bind, and what search
# queries they may perform.
#
# "" is the anonymous dn
[""]
allowed_queries = [
    ["", "base", "(objectclass=*)"],
    ["o=example", "subtree", "(objectclass=*)"],
]

["cn=Administrator"]
# If you don't specify allowed_queries, all queries are granted

["cn=user"]
allowed_queries = [
    ["", "base", "(objectclass=*)"],
]
```

### Redis Cache Configuration

For distributed deployments or when you need cache persistence across restarts:

```toml
bind = "127.0.0.1:3636"
tls_chain = "/tmp/chain.pem"
tls_key = "/tmp/key.pem"

# Cache configuration - Redis backend
[cache]
type = "redis"
url = "redis://localhost:6379"
# Optional: TTL in seconds for cache entries (if not set, entries don't expire)
ttl_seconds = 3600  # 1 hour
# Optional: Custom prefix for Redis keys (default: "ldap_proxy:")
key_prefix = "ldap_proxy:"

ldap_ca = "/tmp/ldap-ca.pem"
ldap_url = "ldaps://idm.example.com"

# ... rest of configuration same as memory cache
```

#### Redis Configuration Options

- **url**: Redis connection URL. Supports:
  - `redis://host:port` - Standard TCP connection
  - `redis://host:port/db` - Specific database number
  - `rediss://host:port` - TLS connection
  - `redis+unix:///path/to/socket` - Unix socket connection
  
- **ttl_seconds** (optional): Time-to-live for cache entries. If not set, entries persist indefinitely (similar to memory cache behavior). Useful for ensuring data freshness.

- **key_prefix** (optional): Prefix for all Redis keys. Default is `ldap_proxy:`. Useful when sharing a Redis instance with other applications.

## Cache Backend Comparison

### Memory Cache

**Pros:**
- Fastest performance (no network overhead)
- Simple configuration
- No external dependencies

**Cons:**
- Cache lost on restart
- Limited to single instance
- Memory usage on the proxy server

**Best for:**
- Single instance deployments
- When maximum performance is critical
- Simple setups without high availability requirements

### Redis Cache

**Pros:**
- Cache persists across restarts (with Redis persistence enabled)
- Shared cache across multiple proxy instances
- Optional TTL for automatic data freshness
- Offloads memory usage from proxy servers
- Better for horizontal scaling

**Cons:**
- Network latency for cache operations
- Additional infrastructure (Redis server)
- More complex setup

**Best for:**
- Multi-instance deployments
- When cache persistence is important
- Load-balanced setups with multiple proxies
- When you need guaranteed data freshness (via TTL)

## How It Works

1. **Normal Operation**: When the backend LDAP server is reachable:
   - All bind and search requests are forwarded to the upstream server
   - Successful search results are stored in the fallback cache
   - Responses are returned from the backend

2. **Fallback Mode**: When the backend LDAP server is unreachable:
   - ldap-proxy automatically serves cached data for previously seen queries
   - Clients experience no interruption in service
   - Log messages indicate fallback mode is active

3. **Recovery**: When the backend becomes available again:
   - ldap-proxy automatically resumes proxying requests
   - Cache is updated with fresh data
   - Normal operation continues

## Where do I get it?

* OpenSUSE: `zypper in ldap-proxy`
* docker: `docker pull firstyear/ldap-proxy:latest`

## Use Cases

- **High Availability**: Protect applications from LDAP server outages
- **Maintenance Windows**: Keep services running while performing backend maintenance
- **Network Resilience**: Handle temporary network partitions gracefully
- **Security Filtering**: Control which DNs can authenticate and what they can query
- **Multi-Tenant Environments**: Isolate different applications with specific query permissions
- **Distributed Deployments**: Use Redis cache for load-balanced proxy instances
- **Cache Persistence**: Maintain cache across proxy restarts with Redis

## Deployment Patterns

### Single Instance (Memory Cache)

```
┌──────────┐         ┌─────────────┐         ┌──────────────┐
│  Client  │ ──────▶ │ ldap-proxy  │ ──────▶ │ LDAP Backend │
└──────────┘         │ (memory)    │         └──────────────┘
                     └─────────────┘
```

Simple, high-performance setup for single proxy deployments.

### Load Balanced (Redis Cache)

```
                     ┌─────────────┐
                  ┌─▶│ ldap-proxy  │─┐
┌──────────┐     │  │  (instance1) │ │      ┌──────────────┐
│  Client  │ ────┤  └─────────────┘ ├────▶ │ LDAP Backend │
└──────────┘     │  ┌─────────────┐ │      └──────────────┘
                  └─▶│ ldap-proxy  │─┘
                     │  (instance2) │
                     └─────────────┘
                            │
                            ▼
                     ┌─────────────┐
                     │    Redis    │
                     │   (shared)  │
                     └─────────────┘
```

Shared cache across multiple proxy instances for high availability and horizontal scaling.

## FAQ

### How long does cached data remain valid?

**Memory Cache**: Cached data never expires. It remains in the fallback cache until:
- The backend becomes reachable again and provides fresh data
- The cache fills up and older entries are evicted (LRU policy)
- The service is restarted

**Redis Cache**: Depends on configuration:
- With `ttl_seconds` set: Entries expire after the specified duration
- Without `ttl_seconds`: Similar to memory cache, entries persist until replaced
- Redis persistence configuration determines survival across Redis restarts

### What happens when the backend is down and there's no cached data?

If a query is performed while the backend is unreachable and there's no cached data for that specific query, ldap-proxy will return an `Unavailable` error with the message "Backend LDAP server unavailable and no cached data".

### How much memory/cache should I allocate?

**Memory Cache**: This depends on:
- The number of unique queries your applications perform
- The size of search results (number of entries and attributes)
- Your desired coverage during outages

The default is 256 MB. For production environments, consider:
- Small deployments: 256 MB - 512 MB
- Medium deployments: 512 MB - 2 GB
- Large deployments: 2 GB - 8 GB+

**Redis Cache**: Size is limited by your Redis server configuration. Consider:
- Redis maxmemory setting
- Redis eviction policy (e.g., `allkeys-lru`)
- Use `ttl_seconds` to automatically expire old entries

Monitor your cache hit rate and adjust accordingly.

### Should I use memory cache or Redis cache?

Choose **memory cache** if:
- You're running a single proxy instance
- Maximum performance is critical
- You don't need cache persistence across restarts
- You want simpler setup and maintenance

Choose **Redis cache** if:
- You're running multiple proxy instances behind a load balancer
- You need cache persistence across proxy restarts
- You want to offload memory usage from proxy servers
- You need guaranteed data freshness via TTL
- You're already running Redis infrastructure

### Can multiple proxy instances share a Redis cache?

Yes! This is one of the primary benefits of using Redis. Multiple ldap-proxy instances can share the same Redis cache, providing:
- Consistent cache hits across all instances
- Reduced load on the backend LDAP server
- Better resource utilization

### Why can't ldap-proxy running under systemd read my certificates?

Because we use systemd dynamic users. This means that ldap-proxy is always isolated in a sandboxed
user, and that user can dynamically change its uid/gid.

To resolve this, you need to add ldap-proxy to have a supplemental group that can read your certs.

```bash
# systemctl edit ldap-proxy
[Service]
SupplementaryGroups=certbot
```

Then restart ldap-proxy. Also be sure to check that the group has proper execute bits along the
directory paths and that the certs are readable to the group!

### Can I use this as a caching layer to reduce backend load?

While ldap-proxy does cache results, it always attempts to query the backend first. It's designed primarily as a fallback/high-availability solution rather than a performance optimization cache. The caching is a side effect of the fallback mechanism.

However, with Redis cache and TTL configured, you can achieve some load reduction:
- Cache hits across multiple proxy instances reduce backend queries
- TTL prevents constant backend queries for the same data within the TTL window
- During backend outages, all load is served from cache

### Does ldap-proxy support HAProxy PROXY protocol?

Yes! Set `remote_ip_addr_info = "ProxyV2"` in your configuration to enable PROXY protocol v2 support. This allows ldap-proxy to receive the real client IP address when running behind HAProxy or similar load balancers.

### What LDAP operations are supported?

- Bind (authentication)
- Search (with query filtering)
- Unbind
- Extended operations (WhoAmI)

Modify operations (add, delete, modify, modifyDN) are not supported as ldap-proxy is designed as a read-only proxy.

### How do I monitor cache performance?

Monitor the logs for:
- "Backend is reachable, updating fallback cache" - Cache is being populated
- "Serving from fallback cache" - Cache hits during backend outages
- "Backend unreachable and no fallback data available" - Cache misses

For Redis, you can also use Redis monitoring tools:
- `redis-cli INFO stats` - See hit/miss ratios
- `redis-cli KEYS ldap_proxy:*` - List cached entries
- Monitor memory usage with `redis-cli INFO memory`

### Can I pre-populate the cache?

Not directly, but you can:
1. Start ldap-proxy with the backend available
2. Execute typical queries your applications will use
3. The cache will be populated with these results
4. For Redis: If configured without TTL, this cache persists across proxy restarts