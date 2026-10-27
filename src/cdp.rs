//! CDP (Chrome DevTools Protocol) endpoint management for `bm open --cdp`.
//!
//! Guarantees, per the release-plan guardrails:
//! - **localhost-only**: the launch argv pins `--remote-debugging-address=127.0.0.1`
//!   explicitly (Chromium also defaults to loopback, but bm does not rely on
//!   defaults for this) and every endpoint we handle is a 127.0.0.1 URL.
//! - **strictly opt-in**: CDP is enabled only when `--cdp` is passed to that
//!   exact launch. Shortcuts, managed policy, and every other bm path never
//!   add the debugging flags.
//! - **no fixed ports**: always `--remote-debugging-port=0` (OS-assigned);
//!   the real port is discovered from `DevToolsActivePort` inside the
//!   profile dir. A fixed port would be a collision-prone, enumerable
//!   target — bm never emits one.
//!
//! A CDP endpoint equals full browser control (sessions, cookies, everything
//! visible in the profile), so every launch that enables it prints a
//! warning, and the endpoint registry `~/.bm/cdp.json` is written 0600.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{BmError, Result};
use crate::storage::StorageManager;

const DEVTOOLS_PORT_FILE: &str = "DevToolsActivePort";
/// Registry file under ~/.bm, mode 0600.
const REGISTRY_FILE: &str = "cdp.json";
/// How long `bm open --cdp` waits for Chromium to publish its endpoint.
const PORT_WAIT: Duration = Duration::from_secs(10);
const PORT_POLL: Duration = Duration::from_millis(200);

/// One live CDP endpoint, keyed `<browser>:<profile>` in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpEntry {
    pub port: u16,
    /// Path part of the browser websocket (line 2 of DevToolsActivePort).
    pub ws_path: String,
    /// The pid bm spawned (may be gone after a singleton handoff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
}

impl CdpEntry {
    /// Loopback browser websocket URL.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}{}", self.port, self.ws_path)
    }
}

/// Registry shape: `<browser>:<profile>` → endpoint.
pub type CdpRegistry = HashMap<String, CdpEntry>;

pub fn registry_path(storage: &StorageManager) -> PathBuf {
    storage.base_dir().join(REGISTRY_FILE)
}

/// Load the endpoint registry; missing or corrupt files read as empty.
pub fn load_registry(storage: &StorageManager) -> CdpRegistry {
    fs::read_to_string(registry_path(storage))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Persist the registry atomically with 0600 permissions — the endpoint is
/// a full-control handle and must never be group/world-readable.
pub fn save_registry(storage: &StorageManager, registry: &CdpRegistry) -> Result<()> {
    let base = storage.base_dir();
    fs::create_dir_all(base)?;
    let tmp = base.join(format!("{REGISTRY_FILE}.tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(serde_json::to_string_pretty(registry)?.as_bytes())?;
    }
    let path = registry_path(storage);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Whether the loopback port accepts TCP connections.
pub fn port_alive(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(250),
    )
    .is_ok()
}

/// Parse `DevToolsActivePort`: line 1 is the port, line 2 the browser ws
/// path (e.g. `/devtools/browser/<uuid>`).
pub fn parse_port_file(content: &str) -> Option<(u16, String)> {
    let mut lines = content.lines();
    let port = lines.next()?.trim().parse().ok()?;
    let ws_path = lines.next()?.trim().to_string();
    (port != 0 && ws_path.starts_with('/')).then_some((port, ws_path))
}

/// Poll the profile dir for a fresh, connectable DevToolsActivePort.
pub fn wait_for_endpoint(profile_dir: &Path) -> Result<CdpEntry> {
    let port_file = profile_dir.join(DEVTOOLS_PORT_FILE);
    let deadline = std::time::Instant::now() + PORT_WAIT;
    loop {
        if let Ok(content) = fs::read_to_string(&port_file) {
            if let Some((port, ws_path)) = parse_port_file(&content) {
                if port_alive(port) {
                    return Ok(CdpEntry {
                        port,
                        ws_path,
                        pid: None,
                        started_at: Utc::now(),
                    });
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(BmError::Other(format!(
                "timed out waiting for {DEVTOOLS_PORT_FILE} in {} — is this a Chromium-based browser, and is the profile already running without --cdp?",
                profile_dir.display()
            )));
        }
        std::thread::sleep(PORT_POLL);
    }
}

/// Graceful shutdown via the CDP `Browser.close` method over the browser
/// websocket — the protocol-sanctioned teardown (session cleanup, no
/// SIGKILL-style data loss).
pub fn close_via_browser_close(entry: &CdpEntry) -> Result<()> {
    use tungstenite::Message;

    let (mut socket, _response) = tungstenite::connect(entry.ws_url())
        .map_err(|e| BmError::Other(format!("cannot connect to {} : {e}", entry.ws_url())))?;

    let request = serde_json::json!({"id": 1, "method": "Browser.close"});
    socket
        .send(Message::text(request.to_string()))
        .map_err(|e| BmError::Other(format!("failed to send Browser.close: {e}")))?;

    // The reply (and then the connection drop) confirm the shutdown path.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(BmError::Other(
                "timed out waiting for the Browser.close reply".into(),
            ));
        }
        // Plain ws:// only — the TLS arm cannot occur for loopback endpoints.
        if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = socket.get_ref() {
            tcp.set_read_timeout(Some(remaining.min(Duration::from_millis(500))))?;
        }
        match socket.read() {
            Ok(Message::Text(msg)) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg) {
                    if v["id"] == serde_json::json!(1) {
                        // Acknowledged; the browser is closing.
                        break;
                    }
                    if let Some(err) = v.get("error") {
                        return Err(BmError::Other(format!("Browser.close failed: {err}")));
                    }
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = socket.send(Message::Pong(p));
            }
            Ok(Message::Close(_)) | Err(_) => break, // connection dropped: closing
            Ok(_) => {}                              // events we don't care about
        }
    }
    let _ = socket.close(None);
    Ok(())
}

/// Remove an entry and persist; a no-op when absent.
pub fn remove_entry(
    storage: &StorageManager,
    browser: crate::browser::Browser,
    name: &str,
) -> Result<bool> {
    let key = format!("{browser}:{name}");
    let mut registry = load_registry(storage);
    let removed = registry.remove(&key).is_some();
    if removed {
        save_registry(storage, &registry)?;
    }
    Ok(removed)
}

/// Drop every registry entry whose port is dead; returns how many were
/// purged and persists only when something changed.
pub fn purge_dead_entries(storage: &StorageManager) -> Result<usize> {
    let mut registry = load_registry(storage);
    let before = registry.len();
    registry.retain(|_, entry| port_alive(entry.port));
    let purged = before - registry.len();
    if purged > 0 {
        save_registry(storage, &registry)?;
    }
    Ok(purged)
}

/// The launch flags enabling CDP: OS-assigned port, loopback binding.
pub fn cdp_launch_flags() -> Vec<String> {
    vec![
        "--remote-debugging-port=0".to_string(),
        "--remote-debugging-address=127.0.0.1".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::Browser;
    use crate::storage::StorageManager;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        fs::create_dir_all(&base).unwrap();
        (tmp, StorageManager::with_base_dir(base))
    }

    #[test]
    fn cdp_flags_pin_os_port_and_loopback() {
        let flags = cdp_launch_flags();
        assert!(flags.contains(&"--remote-debugging-port=0".to_string()));
        assert!(flags.contains(&"--remote-debugging-address=127.0.0.1".to_string()));
        // No fixed port anywhere.
        assert!(!flags
            .iter()
            .any(|f| f.contains(":9222") || f.contains("port=9")));
    }

    #[test]
    fn parse_port_file_accepts_wellformed_and_rejects_the_rest() {
        let (port, path) =
            parse_port_file("45123\n/devtools/browser/abc-123\n").expect("wellformed");
        assert_eq!(port, 45123);
        assert_eq!(path, "/devtools/browser/abc-123");

        assert!(parse_port_file("").is_none());
        assert!(parse_port_file("not-a-port\n/x\n").is_none());
        assert!(
            parse_port_file("0\n/devtools/browser/x\n").is_none(),
            "port 0 is never valid"
        );
        assert!(parse_port_file("45123\nno-leading-slash\n").is_none());
        // Trailing newline / CRLF tolerated.
        assert!(parse_port_file("45123\r\n/devtools/browser/x\r\n").is_some());
    }

    #[test]
    fn registry_roundtrip_is_private_0600() {
        let (_tmp, storage) = setup();
        let entry = CdpEntry {
            port: 40000,
            ws_path: "/devtools/browser/x".into(),
            pid: Some(42),
            started_at: Utc::now(),
        };
        let mut registry = CdpRegistry::new();
        registry.insert("chrome:work".into(), entry);
        save_registry(&storage, &registry).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(registry_path(&storage))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "endpoint registry must be owner-only");
        }
        let loaded = load_registry(&storage);
        assert_eq!(loaded["chrome:work"].port, 40000);
        assert_eq!(
            loaded["chrome:work"].ws_url(),
            "ws://127.0.0.1:40000/devtools/browser/x"
        );

        // Corrupt file reads as empty, never crashes.
        fs::write(registry_path(&storage), "garbage{").unwrap();
        assert!(load_registry(&storage).is_empty());
    }

    #[test]
    fn purge_dead_entries_removes_only_dead_ports() {
        let (_tmp, storage) = setup();
        let mut registry = CdpRegistry::new();
        // A port this test process actually listens on stays.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let live_port = listener.local_addr().unwrap().port();
        registry.insert(
            "chrome:alive".into(),
            CdpEntry {
                port: live_port,
                ws_path: "/x".into(),
                pid: None,
                started_at: Utc::now(),
            },
        );
        // Port 1 on loopback is never listening.
        registry.insert(
            "chrome:dead".into(),
            CdpEntry {
                port: 1,
                ws_path: "/x".into(),
                pid: None,
                started_at: Utc::now(),
            },
        );
        save_registry(&storage, &registry).unwrap();

        let purged = purge_dead_entries(&storage).unwrap();
        assert_eq!(purged, 1);
        let loaded = load_registry(&storage);
        assert!(loaded.contains_key("chrome:alive"));
        assert!(!loaded.contains_key("chrome:dead"));
    }

    #[test]
    fn remove_entry_roundtrip() {
        let (_tmp, storage) = setup();
        let mut registry = CdpRegistry::new();
        registry.insert(
            format!("{}:work", Browser::Chrome),
            CdpEntry {
                port: 40000,
                ws_path: "/x".into(),
                pid: None,
                started_at: Utc::now(),
            },
        );
        save_registry(&storage, &registry).unwrap();

        assert!(remove_entry(&storage, Browser::Chrome, "work").unwrap());
        assert!(
            !remove_entry(&storage, Browser::Chrome, "work").unwrap(),
            "second remove is a no-op"
        );
        assert!(load_registry(&storage).is_empty());
    }
}
