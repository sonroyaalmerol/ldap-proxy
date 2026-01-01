# Ldap Proxy

A fast, simple, in-memory fallback proxy for LDAP that provides high availability and allows limiting of DNs and their searches.

## Overview

ldap-proxy acts as a transparent LDAP proxy with automatic fallback capabilities. It continuously attempts to forward requests to the upstream LDAP server while maintaining a fallback cache. When the backend LDAP server becomes unavailable, ldap-proxy seamlessly serves cached data, ensuring your applications remain operational during outages.

## Features

- **Transparent Proxying**: All requests are forwarded to the upstream LDAP server when available
- **Automatic Fallback**: Seamlessly serves cached data when the backend is unreachable
- **No Cache Expiration**: Fallback data never expires until replaced with fresh data from the backend
- **LDAP Firewall**: Filter which DNs can bind and what queries they may perform
- **High Performance**: In-memory cache with configurable size limits
- **TLS/LDAPS Support**: Secure connections for both client and upstream server

## Configuration

```toml
# /data/config.toml for containers.
# /etc/ldap-proxy/config.toml for packaged versions.

bind = "127.0.0.1:3636"
tls_chain = "/tmp/chain.pem"
tls_key = "/tmp/key.pem"

# Number of bytes to allocate for the fallback cache
# Default: 268435456 (256 MB)
# fallback_cache_bytes = 268435456

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

## FAQ

### How long does cached data remain valid?

Cached data never expires. It remains in the fallback cache until:
- The backend becomes reachable again and provides fresh data
- The cache fills up and older entries are evicted (LRU policy)
- The service is restarted

### What happens when the backend is down and there's no cached data?

If a query is performed while the backend is unreachable and there's no cached data for that specific query, ldap-proxy will return an `Unavailable` error with the message "Backend LDAP server unavailable and no cached data".

### How much memory should I allocate for the fallback cache?

This depends on:
- The number of unique queries your applications perform
- The size of search results (number of entries and attributes)
- Your desired coverage during outages

The default is 256 MB. For production environments, consider:
- Small deployments: 256 MB - 512 MB
- Medium deployments: 512 MB - 2 GB
- Large deployments: 2 GB - 8 GB+

Monitor your cache hit rate and adjust accordingly.

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

### Does ldap-proxy support HAProxy PROXY protocol?

Yes! Set `remote_ip_addr_info = "ProxyV2"` in your configuration to enable PROXY protocol v2 support. This allows ldap-proxy to receive the real client IP address when running behind HAProxy or similar load balancers.

### What LDAP operations are supported?

- Bind (authentication)
- Search (with query filtering)
- Unbind
- Extended operations (WhoAmI)

Modify operations (add, delete, modify, modifyDN) are not supported as ldap-proxy is designed as a read-only proxy.