use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{BmError, Result};

/// Proxy protocol supported by Chromium's `--proxy-server` flag and managed policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyScheme {
    Socks5,
    Http,
    Https,
}

impl ProxyScheme {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProxyScheme::Socks5 => "socks5",
            ProxyScheme::Http => "http",
            ProxyScheme::Https => "https",
        }
    }
}

impl fmt::Display for ProxyScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-profile egress proxy configuration.
///
/// Serialized inside `~/.bm/metadata.json`; `Option<ProxyConfig>` on
/// `ProfileMetadata` keeps older registries (without this field) loadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass: Option<Vec<String>>,
}

impl ProxyConfig {
    /// Parse `<scheme>://<host>:<port>` (IPv6 hosts must be bracketed) plus an
    /// optional bypass list string with comma- or semicolon-separated rules.
    pub fn parse(server: &str, bypass: Option<&str>) -> Result<Self> {
        let server = server.trim();
        let (scheme_str, rest) = server
            .split_once("://")
            .ok_or_else(|| invalid_proxy(format!("missing scheme in '{server}', expected <scheme>://<host>:<port> (socks5, http, or https)")))?;

        let scheme = match scheme_str.to_ascii_lowercase().as_str() {
            "socks5" => ProxyScheme::Socks5,
            "http" => ProxyScheme::Http,
            "https" => ProxyScheme::Https,
            other => {
                return Err(invalid_proxy(format!(
                    "unsupported scheme '{other}://' (use socks5, http, or https)"
                )))
            }
        };

        let (host, port_str) = if let Some(inner) = rest.strip_prefix('[') {
            let close = inner
                .find(']')
                .ok_or_else(|| invalid_proxy("unterminated '[' in IPv6 host"))?;
            let port_part = &inner[close + 1..];
            let port = port_part
                .strip_prefix(':')
                .ok_or_else(|| invalid_proxy("expected ':' after bracketed IPv6 host"))?;
            (inner[..close].to_string(), port)
        } else {
            let idx = rest
                .rfind(':')
                .ok_or_else(|| invalid_proxy("missing port, expected <scheme>://<host>:<port>"))?;
            (rest[..idx].to_string(), &rest[idx + 1..])
        };

        Self::validate_host(&host)?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| invalid_proxy(format!("invalid port '{port_str}' (1-65535)")))?;
        if port == 0 {
            return Err(invalid_proxy("port must be between 1 and 65535"));
        }

        Ok(ProxyConfig {
            scheme,
            host,
            port,
            bypass: Self::parse_bypass(bypass)?,
        })
    }

    /// `socks5://host:port` — the form used by `--proxy-server` and the
    /// Chromium `ProxyServer` managed policy.
    pub fn server_string(&self) -> String {
        if self.host.contains(':') {
            format!("{}://[{}]:{}", self.scheme, self.host, self.port)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }

    /// Bypass rules joined for `--proxy-bypass-list` / `ProxyBypassList`
    /// (Chromium accepts both comma- and semicolon-delimited rules).
    pub fn bypass_string(&self) -> Option<String> {
        self.bypass
            .as_ref()
            .filter(|rules| !rules.is_empty())
            .map(|rules| rules.join(","))
    }

    fn validate_host(host: &str) -> Result<()> {
        if host.is_empty() {
            return Err(invalid_proxy("host is empty"));
        }
        if host
            .chars()
            .any(|c| c.is_whitespace() || c == '/' || c == '\\' || c == '@')
        {
            return Err(invalid_proxy(format!("invalid host '{host}'")));
        }
        Ok(())
    }

    fn parse_bypass(bypass: Option<&str>) -> Result<Option<Vec<String>>> {
        let Some(raw) = bypass else {
            return Ok(None);
        };
        let rules: Vec<String> = raw
            .split([',', ';'])
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if rules.is_empty() {
            return Ok(None);
        }
        if let Some(bad) = rules.iter().find(|r| r.chars().any(|c| c.is_whitespace())) {
            return Err(invalid_proxy(format!("invalid bypass rule '{bad}'")));
        }
        Ok(Some(rules))
    }
}

impl fmt::Display for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.server_string())
    }
}

fn invalid_proxy(msg: impl Into<String>) -> BmError {
    BmError::InvalidProxy(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_schemes() {
        let p = ProxyConfig::parse("socks5://127.0.0.1:1080", None).unwrap();
        assert_eq!(p.scheme, ProxyScheme::Socks5);
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 1080);
        assert_eq!(p.bypass, None);
        assert_eq!(p.server_string(), "socks5://127.0.0.1:1080");

        let p = ProxyConfig::parse("http://proxy.corp.example:8080", None).unwrap();
        assert_eq!(p.scheme, ProxyScheme::Http);

        let p = ProxyConfig::parse("HTTPS://Proxy.Example:443 ", None).unwrap();
        assert_eq!(p.scheme, ProxyScheme::Https);
        assert_eq!(p.host, "Proxy.Example");
    }

    #[test]
    fn parse_ipv6_bracketed_host() {
        let p = ProxyConfig::parse("socks5://[::1]:1080", None).unwrap();
        assert_eq!(p.host, "::1");
        assert_eq!(p.server_string(), "socks5://[::1]:1080");
    }

    #[test]
    fn parse_bypass_list() {
        let p = ProxyConfig::parse(
            "http://p:8080",
            Some("localhost, 127.0.0.1 ; *.corp.example,"),
        )
        .unwrap();
        assert_eq!(
            p.bypass_string(),
            Some("localhost,127.0.0.1,*.corp.example".to_string())
        );

        // Empty / whitespace-only bypass yields None
        let p = ProxyConfig::parse("http://p:8080", Some(" , ; ")).unwrap();
        assert_eq!(p.bypass, None);
        assert_eq!(p.bypass_string(), None);

        // Rules may not contain whitespace
        assert!(ProxyConfig::parse("http://p:8080", Some("local host")).is_err());
    }

    #[test]
    fn parse_rejects_invalid_input() {
        assert!(ProxyConfig::parse("127.0.0.1:1080", None).is_err()); // no scheme
        assert!(ProxyConfig::parse("ftp://h:21", None).is_err()); // unsupported scheme
        assert!(ProxyConfig::parse("socks5://127.0.0.1", None).is_err()); // no port
        assert!(ProxyConfig::parse("socks5://127.0.0.1:0", None).is_err()); // port 0
        assert!(ProxyConfig::parse("socks5://127.0.0.1:99999", None).is_err()); // out of range
        assert!(ProxyConfig::parse("socks5://127.0.0.1:abc", None).is_err()); // not numeric
        assert!(ProxyConfig::parse("socks5:// :1080", None).is_err()); // empty host
        assert!(ProxyConfig::parse("socks5://bad host:1080", None).is_err()); // whitespace in host
        assert!(ProxyConfig::parse("socks5://[::1:1080", None).is_err()); // unterminated bracket
    }

    #[test]
    fn serde_roundtrip() {
        let p = ProxyConfig::parse("socks5://10.0.0.1:1080", Some("localhost;*.lan")).unwrap();
        let json = serde_json::to_string(&p).unwrap();
        let back: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert!(json.contains("\"socks5\""));
    }
}
