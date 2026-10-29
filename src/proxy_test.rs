//! Egress verification for proxied profiles (`bm proxy test`).
//!
//! The HTTP client is abstracted behind [`HttpFetcher`] so the verify logic
//! is unit-testable without any network; [`UreqFetcher`] is the real
//! implementation (pure-Rust ureq with the socks-proxy feature, rustls TLS).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;
use crate::storage::StorageManager;

/// One GET, optionally through a proxy server (`<scheme>://<host>:<port>`).
/// Returns the response body (at most 512 bytes) or a human-readable error.
pub trait HttpFetcher {
    fn fetch(
        &self,
        url: &str,
        proxy_server: Option<&str>,
        timeout: Duration,
    ) -> std::result::Result<String, String>;
}

/// Real HTTP client: ureq 2.x. Chosen over reqwest to stay pure-Rust with no
/// tokio runtime; the socks-proxy feature covers SOCKS5, HTTP and HTTPS
/// proxies alike.
pub struct UreqFetcher;

impl HttpFetcher for UreqFetcher {
    fn fetch(
        &self,
        url: &str,
        proxy_server: Option<&str>,
        timeout: Duration,
    ) -> std::result::Result<String, String> {
        let mut builder = ureq::AgentBuilder::new().timeout(timeout);
        if let Some(proxy) = proxy_server {
            let p = ureq::Proxy::new(proxy).map_err(|e| format!("invalid proxy '{proxy}': {e}"))?;
            builder = builder.proxy(p);
        }
        let resp = builder
            .build()
            .get(url)
            .call()
            .map_err(describe_ureq_error)?;
        resp.into_string()
            .map_err(|e| format!("could not read response body: {e}"))
    }
}

/// Normalize an echo answer: trim whitespace and accept at most 512 bytes.
fn normalize_body(body: String) -> String {
    body.trim().chars().take(512).collect()
}

/// Map transport failures to likely causes: connection refused usually means
/// the proxy is down; a timeout usually means egress is blocked.
fn describe_ureq_error(e: ureq::Error) -> String {
    let raw = e.to_string();
    let hint = if raw.contains("refused") {
        "the proxy may be down or the port wrong"
    } else if raw.contains("timed out") || raw.contains("timeout") {
        "egress may be blocked (firewall) or the proxy too slow"
    } else if raw.contains("dns") || raw.contains("resolve") {
        "DNS failure — check the proxy host"
    } else if raw.contains("proxy") {
        "the proxy rejected the connection (auth? wrong scheme?)"
    } else {
        "check the proxy server and your network"
    };
    format!("{raw} ({hint})")
}

/// Outcome of one profile's egress verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressCheck {
    /// IP observed through the proxy (None when the proxied request failed).
    pub exit_ip: Option<String>,
    /// IP observed without any proxy (None when the direct request failed).
    pub direct_ip: Option<String>,
    /// Human-readable failure cause of the proxied request, if any.
    pub through_error: Option<String>,
    /// Human-readable failure cause of the direct request, if any.
    pub direct_error: Option<String>,
    /// Latency of the proxied request in milliseconds.
    pub latency_ms: u128,
    pub through_ok: bool,
    /// Whether the egress provably goes through the proxy. `None` when the
    /// comparison is impossible (either request failed).
    pub effective: Option<bool>,
}

/// Verify one profile's proxy by fetching `url` twice — through the proxy
/// and directly — and comparing the observed IPs.
pub fn check_egress(
    fetcher: &dyn HttpFetcher,
    url: &str,
    proxy: &ProxyConfig,
    timeout: Duration,
) -> EgressCheck {
    let start = Instant::now();
    let through = fetcher.fetch(url, Some(&proxy.server_string()), timeout);
    let latency_ms = start.elapsed().as_millis();

    let direct = fetcher.fetch(url, None, timeout);

    let through_error = through.as_ref().err().cloned();
    let direct_error = direct.as_ref().err().cloned();
    let exit_ip = through.ok().map(normalize_body);
    let direct_ip = direct.ok().map(normalize_body);
    let through_ok = exit_ip.is_some();
    // Effective egress = the proxied path observed a DIFFERENT IP than the
    // direct path. Equal IPs mean the proxy is being bypassed.
    let effective = match (&exit_ip, &direct_ip) {
        (Some(exit), Some(direct)) => Some(exit != direct),
        _ => None,
    };

    EgressCheck {
        exit_ip,
        direct_ip,
        through_error,
        direct_error,
        latency_ms,
        through_ok,
        effective,
    }
}

/// Resolve the profiles to verify: the named profile (possibly proxy-free —
/// the caller reports that), or every profile with a proxy configured.
/// Errors when the named profile is missing or when no profile carries a
/// proxy. Returns `(name, proxy)` pairs.
pub fn resolve_targets(
    storage: &StorageManager,
    browser: Browser,
    name: Option<&str>,
) -> Result<Vec<(String, Option<ProxyConfig>)>> {
    match name {
        Some(name) => {
            if !storage.profile_dir(browser, name).exists() {
                return Err(BmError::ProfileNotFound {
                    name: name.to_string(),
                    browser: browser.display_name().to_string(),
                });
            }
            Ok(vec![(name.to_string(), storage.proxy_for(browser, name))])
        }
        None => {
            let targets: Vec<(String, Option<ProxyConfig>)> = storage
                .list_profiles(browser)?
                .into_iter()
                .filter(|(m, _)| m.proxy.is_some())
                .map(|(m, _)| (m.name, m.proxy))
                .collect();
            if targets.is_empty() {
                return Err(BmError::Other(
                    "No profile has a proxy configured. Set one with: bm proxy set <name> --server <scheme>://<host>:<port>"
                        .into(),
                ));
            }
            Ok(targets)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Canned fetcher: maps the proxy argument to a body or an error; records calls.
    struct MockFetcher {
        responses: Vec<(Option<String>, std::result::Result<String, String>)>,
        calls: RefCell<Vec<Option<String>>>,
    }

    impl HttpFetcher for MockFetcher {
        fn fetch(
            &self,
            _url: &str,
            proxy_server: Option<&str>,
            _timeout: Duration,
        ) -> std::result::Result<String, String> {
            self.calls
                .borrow_mut()
                .push(proxy_server.map(str::to_string));
            for (target, result) in &self.responses {
                if *target == proxy_server.map(str::to_string) {
                    return result.clone();
                }
            }
            Err(format!("no mock for {:?}", proxy_server))
        }
    }

    fn proxy() -> ProxyConfig {
        ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap()
    }

    #[test]
    fn effective_when_exit_ip_differs() {
        let mock = MockFetcher {
            responses: vec![
                (
                    Some("socks5://10.0.0.1:1080".into()),
                    Ok("203.0.113.7\n".into()),
                ),
                (None, Ok("198.51.100.2".into())),
            ],
            calls: RefCell::new(vec![]),
        };
        let check = check_egress(
            &mock,
            "https://api.ipify.org",
            &proxy(),
            Duration::from_secs(5),
        );
        assert_eq!(check.exit_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(check.direct_ip.as_deref(), Some("198.51.100.2"));
        assert_eq!(check.effective, Some(true));
        assert!(check.through_ok);
        // Two calls: first through the proxy, then direct.
        assert_eq!(
            *mock.calls.borrow(),
            vec![Some("socks5://10.0.0.1:1080".into()), None]
        );
    }

    #[test]
    fn ineffective_when_exit_ip_equals_direct() {
        let mock = MockFetcher {
            responses: vec![
                (
                    Some("socks5://10.0.0.1:1080".into()),
                    Ok("198.51.100.2".into()),
                ),
                (None, Ok("198.51.100.2".into())),
            ],
            calls: RefCell::new(vec![]),
        };
        let check = check_egress(
            &mock,
            "https://api.ipify.org",
            &proxy(),
            Duration::from_secs(5),
        );
        assert_eq!(check.effective, Some(false));
    }

    #[test]
    fn proxied_failure_is_reported_without_comparison() {
        let mock = MockFetcher {
            responses: vec![
                (
                    Some("socks5://10.0.0.1:1080".into()),
                    Err("Connection refused — the proxy may be down or the port wrong".into()),
                ),
                (None, Ok("198.51.100.2".into())),
            ],
            calls: RefCell::new(vec![]),
        };
        let check = check_egress(
            &mock,
            "https://api.ipify.org",
            &proxy(),
            Duration::from_secs(5),
        );
        assert!(!check.through_ok);
        assert_eq!(check.exit_ip, None);
        assert_eq!(check.effective, None, "no comparison possible");
        assert!(check.through_error.unwrap().contains("proxy may be down"));
    }

    #[test]
    fn bodies_are_trimmed_and_capped_at_512_chars() {
        let long: String = "x".repeat(600);
        let mock = MockFetcher {
            responses: vec![(Some("socks5://10.0.0.1:1080".into()), Ok(long))],
            calls: RefCell::new(vec![]),
        };
        let check = check_egress(
            &mock,
            "https://api.ipify.org",
            &proxy(),
            Duration::from_secs(5),
        );
        assert_eq!(check.exit_ip.unwrap().len(), 512);
    }

    #[test]
    fn resolve_targets_name_vs_all() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
        let browser = Browser::Chrome;

        // Named but missing profile → ProfileNotFound.
        assert!(resolve_targets(&storage, browser, Some("ghost")).is_err());

        storage.create_profile("work", None, browser).unwrap();
        storage.create_profile("plain", None, browser).unwrap();

        // Named profile without proxy resolves fine (caller reports it).
        let targets = resolve_targets(&storage, browser, Some("work")).unwrap();
        assert_eq!(targets[0].0, "work");
        assert_eq!(targets[0].1, None);

        // No name + no proxied profiles anywhere → hint error.
        assert!(resolve_targets(&storage, browser, None).is_err());

        let cfg = ProxyConfig::parse("http://p:8080", None).unwrap();
        storage.set_proxy("work", browser, Some(cfg)).unwrap();
        let targets = resolve_targets(&storage, browser, None).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, "work");
    }
}
