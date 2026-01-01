// use ldap_proxy::proxy::BasicLdapClient;

use ldap3_proto::proto::LdapResult;
use ldap_proxy::proxy::CachedValue;
use ldap_proxy::Config;
use std::time::Instant;

#[test]
fn test_config_load() {
    assert!(toml::from_str::<Config>("").is_err());

    let load_config = toml::from_str::<Config>(include_str!("test_config.toml"));
    assert!(load_config.is_ok());

    let config = load_config.expect("Failed to load config");
    assert_eq!(
        config.ldap_ca.to_str(),
        Some("/etc/ldap-proxy/ldap-ca.pem")
    );
    
    // Test default fallback_cache_bytes value
    assert_eq!(config.fallback_cache_bytes, 268435456); // 256 MB
}

#[test]
fn test_config_custom_cache_size() {
    let config_str = r#"
        bind = "127.0.0.1:3636"
        tls_chain = "/etc/ldap-proxy/chain.pem"
        tls_key = "/etc/ldap-proxy/key.pem"
        ldap_ca = "/etc/ldap-proxy/ldap-ca.pem"
        ldap_url = "ldaps://ldap.example.com"
        fallback_cache_bytes = 536870912
    "#;
    
    let config = toml::from_str::<Config>(config_str).expect("Failed to parse config");
    assert_eq!(config.fallback_cache_bytes, 536870912); // 512 MB
}

#[test]
fn test_config_allow_all_bind_dns() {
    let config_str = r#"
        bind = "127.0.0.1:3636"
        tls_chain = "/etc/ldap-proxy/chain.pem"
        tls_key = "/etc/ldap-proxy/key.pem"
        ldap_ca = "/etc/ldap-proxy/ldap-ca.pem"
        ldap_url = "ldaps://ldap.example.com"
        allow_all_bind_dns = true
    "#;
    
    let config = toml::from_str::<Config>(config_str).expect("Failed to parse config");
    assert!(config.allow_all_bind_dns);
}

#[test]
fn test_cachedvalue() {
    let cv = CachedValue {
        cached_at: Instant::now(),
        entries: Vec::with_capacity(5),
        result: LdapResult {
            code: ldap3_proto::LdapResultCode::Busy,
            matcheddn: "dn=doo".to_string(),
            message: "ohno".to_string(),
            referral: Vec::with_capacity(5),
        },
        ctrl: Vec::with_capacity(5),
    };
    assert_eq!(cv.size(), 144);
}

#[test]
fn test_cachedvalue_size_calculation() {
    use ldap3_proto::proto::{LdapSearchResultEntry, LdapPartialAttribute};
    
    let mut entries = Vec::new();
    entries.push((
        LdapSearchResultEntry {
            dn: "cn=test,dc=example,dc=com".to_string(),
            attributes: vec![
                LdapPartialAttribute {
                    atype: "cn".to_string(),
                    vals: vec![b"test".to_vec()],
                },
            ],
        },
        Vec::new(),
    ));
    
    let cv = CachedValue {
        cached_at: Instant::now(),
        entries,
        result: LdapResult {
            code: ldap3_proto::LdapResultCode::Success,
            matcheddn: "".to_string(),
            message: "".to_string(),
            referral: Vec::new(),
        },
        ctrl: Vec::new(),
    };
    
    // Size should be greater than base struct size due to entry data
    assert!(cv.size() > std::mem::size_of::<CachedValue>());
}

#[test]
fn test_binddn_map_parsing() {
    let load_config = toml::from_str::<Config>(include_str!("test_config.toml"));
    assert!(load_config.is_ok());
    
    let config = load_config.expect("Failed to load config");
    
    // Check anonymous DN exists
    assert!(config.binddn_map.contains_key(""));
    
    // Check John Cena DN exists and has queries
    let john_cena_dn = "cn=John Cena,dc=dooo,dc=do,dc=do,dc=doooooo";
    assert!(config.binddn_map.contains_key(john_cena_dn));
    let john_config = config.binddn_map.get(john_cena_dn).unwrap();
    assert_eq!(john_config.allowed_queries.len(), 2);
    
    // Check Administrator DN exists with no query restrictions
    assert!(config.binddn_map.contains_key("cn=Administrator"));
    let admin_config = config.binddn_map.get("cn=Administrator").unwrap();
    assert_eq!(admin_config.allowed_queries.len(), 0);
}

#[test]
fn test_remote_ip_addr_info_parsing() {
    let config_none = r#"
        bind = "127.0.0.1:3636"
        tls_chain = "/etc/ldap-proxy/chain.pem"
        tls_key = "/etc/ldap-proxy/key.pem"
        ldap_ca = "/etc/ldap-proxy/ldap-ca.pem"
        ldap_url = "ldaps://ldap.example.com"
        remote_ip_addr_info = "None"
    "#;
    
    let config = toml::from_str::<Config>(config_none).expect("Failed to parse config");
    assert!(matches!(config.remote_ip_addr_info, ldap_proxy::AddrInfoSource::None));
    
    let config_proxy = r#"
        bind = "127.0.0.1:3636"
        tls_chain = "/etc/ldap-proxy/chain.pem"
        tls_key = "/etc/ldap-proxy/key.pem"
        ldap_ca = "/etc/ldap-proxy/ldap-ca.pem"
        ldap_url = "ldaps://ldap.example.com"
        remote_ip_addr_info = "ProxyV2"
    "#;
    
    let config = toml::from_str::<Config>(config_proxy).expect("Failed to parse config");
    assert!(matches!(config.remote_ip_addr_info, ldap_proxy::AddrInfoSource::ProxyV2));
}