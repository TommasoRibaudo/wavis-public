use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use ipnet::IpNet;
use std::env;
use std::net::{IpAddr, SocketAddr};

/// Configuration for client IP extraction.
#[derive(Clone)]
pub struct IpConfig {
    /// When true, trust the X-Forwarded-For header (for reverse proxy deployments).
    /// Default: false.
    pub trust_proxy_headers: bool,
    /// When non-empty, only trust XFF from connections whose remote IP falls
    /// within one of these CIDRs. Empty = trust any source (backward compat).
    pub trusted_proxy_cidrs: Vec<IpNet>,
}

impl IpConfig {
    /// Read configuration from environment variables.
    /// `TRUST_PROXY_HEADERS=true` enables proxy header trust; anything else (or absent) defaults to false.
    /// `TRUSTED_PROXY_CIDRS` is a comma-separated list of CIDR ranges (e.g. "10.0.0.0/8,172.16.0.0/12").
    pub fn from_env() -> Self {
        let trust_proxy_headers = env::var("TRUST_PROXY_HEADERS")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        let trusted_proxy_cidrs: Vec<IpNet> = env::var("TRUSTED_PROXY_CIDRS")
            .ok()
            .map(|v| {
                v.split(',')
                    .filter_map(|s| {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            return None;
                        }
                        match trimmed.parse::<IpNet>() {
                            Ok(net) => Some(net),
                            Err(e) => {
                                tracing::warn!(
                                    cidr = trimmed,
                                    error = %e,
                                    "ignoring unparseable TRUSTED_PROXY_CIDRS entry"
                                );
                                None
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        if trust_proxy_headers && trusted_proxy_cidrs.is_empty() {
            tracing::warn!(
                "TRUST_PROXY_HEADERS=true but TRUSTED_PROXY_CIDRS is empty — \
                 XFF will be trusted from ANY source IP. Set TRUSTED_PROXY_CIDRS \
                 to restrict to known proxy ranges."
            );
        }

        Self {
            trust_proxy_headers,
            trusted_proxy_cidrs,
        }
    }
}

/// Returns true if the remote IP is trusted to provide XFF headers.
fn is_trusted_proxy(remote_ip: IpAddr, config: &IpConfig) -> bool {
    if config.trusted_proxy_cidrs.is_empty() {
        return true; // no allowlist = trust all (backward compat)
    }
    config
        .trusted_proxy_cidrs
        .iter()
        .any(|cidr| cidr.contains(&remote_ip))
}

/// Extract the client IP address from the request.
///
/// - When `config.trust_proxy_headers` is `true` and the remote IP is a trusted proxy:
///   reads the `X-Forwarded-For` header and returns the leftmost (client-facing) IP.
///   Falls back to the remote address if the header is absent or contains no parseable IP.
/// - When `config.trust_proxy_headers` is `false`, or the remote IP is not in
///   `trusted_proxy_cidrs`: always uses the direct connection's remote address,
///   ignoring any proxy headers.
pub fn extract_client_ip(
    connect_info: &ConnectInfo<SocketAddr>,
    headers: &HeaderMap,
    config: &IpConfig,
) -> IpAddr {
    let remote_ip = connect_info.0.ip();

    if config.trust_proxy_headers
        && is_trusted_proxy(remote_ip, config)
        && let Some(forwarded_for) = headers.get("x-forwarded-for")
        && let Ok(value) = forwarded_for.to_str()
    {
        // X-Forwarded-For: client, proxy1, proxy2 — leftmost is the original client
        if let Some(leftmost) = value.split(',').next()
            && let Ok(ip) = leftmost.trim().parse::<IpAddr>()
        {
            return ip;
        }
    }
    remote_ip
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::http::HeaderMap;
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn make_connect_info(ip: IpAddr) -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::new(ip, 12345))
    }

    fn headers_with_xff(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", value.parse().unwrap());
        headers
    }

    // Feature: invite-code-hardening, Property 12: IP extraction respects trust_proxy_headers
    // Validates: Requirements 7.1, 7.2, 7.3

    #[test]
    fn test_no_proxy_uses_remote_addr() {
        // Req 7.1: default uses connection remote address
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ci = make_connect_info(remote);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let config = IpConfig {
            trust_proxy_headers: false,
            trusted_proxy_cidrs: vec![],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(
            ip, remote,
            "should use remote addr when trust_proxy_headers=false"
        );
    }

    #[test]
    fn test_proxy_trust_reads_xff() {
        // Req 7.2: when trust_proxy_headers=true, use X-Forwarded-For leftmost IP
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("203.0.113.5, 10.0.0.1");
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec![],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(ip, "203.0.113.5".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_proxy_trust_no_xff_falls_back_to_remote() {
        // Req 7.2: fallback to remote addr when X-Forwarded-For is absent
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ci = make_connect_info(remote);
        let headers = HeaderMap::new();
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec![],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(ip, remote);
    }

    #[test]
    fn test_proxy_trust_unparseable_xff_falls_back_to_remote() {
        // Req 7.2: fallback to remote addr when X-Forwarded-For is unparseable
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("not-an-ip, 10.0.0.1");
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec![],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(ip, remote);
    }

    #[test]
    fn test_trust_false_ignores_xff() {
        // Req 7.3: when trust_proxy_headers=false, X-Forwarded-For is ignored
        let remote = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("8.8.8.8");
        let config = IpConfig {
            trust_proxy_headers: false,
            trusted_proxy_cidrs: vec![],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(ip, remote);
    }

    // --- CIDR allowlist tests ---

    #[test]
    fn test_trusted_cidr_allows_xff() {
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("203.0.113.5");
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec!["10.0.0.0/8".parse().unwrap()],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(
            ip,
            "203.0.113.5".parse::<IpAddr>().unwrap(),
            "XFF should be trusted when remote IP is in trusted CIDR"
        );
    }

    #[test]
    fn test_untrusted_cidr_ignores_xff() {
        let remote = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("8.8.8.8");
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec!["10.0.0.0/8".parse().unwrap()],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(
            ip, remote,
            "XFF should be ignored when remote IP is NOT in trusted CIDR"
        );
    }

    #[test]
    fn test_multiple_trusted_cidrs() {
        let remote = IpAddr::V4(Ipv4Addr::new(172, 16, 0, 5));
        let ci = make_connect_info(remote);
        let headers = headers_with_xff("1.2.3.4");
        let config = IpConfig {
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec![
                "10.0.0.0/8".parse().unwrap(),
                "172.16.0.0/12".parse().unwrap(),
            ],
        };

        let ip = extract_client_ip(&ci, &headers, &config);
        assert_eq!(
            ip,
            "1.2.3.4".parse::<IpAddr>().unwrap(),
            "XFF should be trusted when remote IP matches any trusted CIDR"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        // Property 12: IP extraction respects trust_proxy_headers
        // Validates: Requirements 7.2, 7.3
        #[test]
        fn prop_trust_false_always_uses_remote(
            a in 0u8..=255,
            b in 0u8..=255,
            c in 0u8..=255,
            d in 0u8..=255,
            xa in 1u8..=254,
            xb in 1u8..=254,
            xc in 1u8..=254,
            xd in 1u8..=254,
        ) {
            let remote = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
            let xff_ip = IpAddr::V4(Ipv4Addr::new(xa, xb, xc, xd));
            let ci = make_connect_info(remote);
            let headers = headers_with_xff(&xff_ip.to_string());
            let config = IpConfig { trust_proxy_headers: false, trusted_proxy_cidrs: vec![] };

            let ip = extract_client_ip(&ci, &headers, &config);
            prop_assert_eq!(ip, remote, "trust_proxy_headers=false must always return remote addr");
        }

        // Property 12 (trust=true, no CIDR): when X-Forwarded-For is present and valid, return its leftmost IP
        // Validates: Requirements 7.2
        #[test]
        fn prop_trust_true_uses_xff_when_valid(
            a in 0u8..=255,
            b in 0u8..=255,
            c in 0u8..=255,
            d in 0u8..=255,
            ra in 0u8..=255,
            rb in 0u8..=255,
            rc in 0u8..=255,
            rd in 0u8..=255,
        ) {
            let xff_ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
            let remote = IpAddr::V4(Ipv4Addr::new(ra, rb, rc, rd));
            let ci = make_connect_info(remote);
            let headers = headers_with_xff(&xff_ip.to_string());
            let config = IpConfig { trust_proxy_headers: true, trusted_proxy_cidrs: vec![] };

            let ip = extract_client_ip(&ci, &headers, &config);
            prop_assert_eq!(ip, xff_ip, "trust_proxy_headers=true (no CIDR) must return leftmost XFF IP");
        }

        // Property: untrusted proxy CIDR ignores XFF regardless of trust flag
        #[test]
        fn prop_untrusted_proxy_ignores_xff(
            _ra in 0u8..=255,
            _rb in 0u8..=255,
            rc in 0u8..=255,
            rd in 0u8..=255,
            xa in 1u8..=254,
            xb in 1u8..=254,
            xc in 1u8..=254,
            xd in 1u8..=254,
        ) {
            // Remote is in 192.168.x.x, trusted CIDR is 10.0.0.0/8 — never matches
            let remote = IpAddr::V4(Ipv4Addr::new(192, 168, rc, rd));
            let xff_ip = IpAddr::V4(Ipv4Addr::new(xa, xb, xc, xd));
            let ci = make_connect_info(remote);
            let headers = headers_with_xff(&xff_ip.to_string());
            let config = IpConfig {
                trust_proxy_headers: true,
                trusted_proxy_cidrs: vec!["10.0.0.0/8".parse().unwrap()],
            };

            let ip = extract_client_ip(&ci, &headers, &config);
            prop_assert_eq!(ip, remote,
                "untrusted proxy source must return remote addr, not XFF");
        }
    }
}
