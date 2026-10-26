use std::process::Command;

use crate::browser::Browser;
use crate::error::Result;
use crate::proxy::ProxyConfig;
use crate::storage::StorageManager;

/// Chromium managed-policy keys written by bm. While a proxied profile is
/// active, bm owns these keys in the browser's policy domain; they are
/// removed again when the active profile has no proxy.
pub const MANAGED_POLICY_KEYS: [&str; 4] = [
    "ProxyMode",
    "ProxyServer",
    "ProxyBypassList",
    "WebRtcIPHandlingPolicy",
];

/// Marker written into bm-generated .desktop overrides on Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const DESKTOP_OVERRIDE_MARKER: &str = "# Managed by bm proxy";

/// Apply or clear the global-launch proxy so it matches the currently active
/// profile. Called after `bm switch` and after `bm proxy set/clear` on the
/// active profile. Best-effort: failures warn loudly on stderr but never
/// abort the command that triggered the sync (the profile switch itself has
/// already happened at that point).
pub fn sync_for_active_profile(storage: &StorageManager, browser: Browser) -> Result<()> {
    let proxy = match storage.get_current_active(browser)? {
        // Only bm-managed activations carry a proxy; external symlinks fall
        // back to "no proxy" so bm never leaves stale policy behind.
        Some(name) if !name.starts_with("external (") => storage
            .list_profiles(browser)?
            .into_iter()
            .find(|(m, _)| m.name == name)
            .and_then(|(m, _)| m.proxy),
        _ => None,
    };

    apply(browser, proxy.as_ref())
}

/// macOS `defaults` argv vectors that apply (proxy present) or clear (proxy
/// absent) the proxy managed policy for the given browser policy domain.
/// Pure so it is unit-testable on every platform.
pub fn macos_policy_commands(proxy: Option<&ProxyConfig>, domain: &str) -> Vec<Vec<String>> {
    let domain = domain.to_string();

    match proxy {
        Some(p) => {
            let mut cmds = vec![
                vec![
                    "write".into(),
                    domain.clone(),
                    "ProxyMode".into(),
                    "-string".into(),
                    "fixed_servers".into(),
                ],
                vec![
                    "write".into(),
                    domain.clone(),
                    "ProxyServer".into(),
                    "-string".into(),
                    p.server_string(),
                ],
            ];
            if let Some(bypass) = p.bypass_string() {
                cmds.push(vec![
                    "write".into(),
                    domain.clone(),
                    "ProxyBypassList".into(),
                    "-string".into(),
                    bypass,
                ]);
            }
            cmds.push(vec![
                "write".into(),
                domain,
                "WebRtcIPHandlingPolicy".into(),
                "-string".into(),
                "disable_non_proxied_udp".into(),
            ]);
            cmds
        }
        None => MANAGED_POLICY_KEYS
            .iter()
            .map(|key| vec!["delete".into(), domain.clone(), key.to_string()])
            .collect(),
    }
}

/// Append proxy launch flags to every `Exec=` line of a .desktop entry
/// (main entry plus desktop actions). Pure so it is unit-testable on every
/// platform. `flags` must start with a space (see `proxy_launch_flags`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn apply_proxy_to_desktop_exec(content: &str, flags: &str) -> String {
    let mut out = String::with_capacity(content.len() + flags.len() * 4);
    for line in content.lines() {
        match line.strip_prefix("Exec=") {
            Some(exec) => {
                out.push_str("Exec=");
                out.push_str(exec);
                out.push_str(flags);
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

#[cfg(target_os = "macos")]
fn apply(browser: Browser, proxy: Option<&ProxyConfig>) -> Result<()> {
    apply_policy(browser, proxy, None)
}

/// `domain_override` routes the defaults commands at a scratch domain;
/// used by tests so they never touch a real browser's policy.
#[cfg(target_os = "macos")]
fn apply_policy(
    browser: Browser,
    proxy: Option<&ProxyConfig>,
    domain_override: Option<&str>,
) -> Result<()> {
    let domain = domain_override.unwrap_or(browser.policy_domain());
    for argv in macos_policy_commands(proxy, domain) {
        let deleting = argv.first().map(|c| c == "delete").unwrap_or(false);
        match Command::new("defaults").args(&argv).output() {
            Ok(out) if out.status.success() => {}
            // Deleting an absent key exits non-zero; that is the normal case
            // when the previous profile had no proxy.
            Ok(_) if deleting => {}
            Ok(out) => warn_failed(
                &argv.join(" "),
                String::from_utf8_lossy(&out.stderr).trim(),
            ),
            Err(e) => warn_failed(&argv.join(" "), &e.to_string()),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply(browser: Browser, proxy: Option<&ProxyConfig>) -> Result<()> {
    sync_linux_desktop(browser, proxy)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn apply(_browser: Browser, _proxy: Option<&ProxyConfig>) -> Result<()> {
    Ok(())
}

fn warn_failed(command: &str, detail: &str) {
    eprintln!(
        "Warning: proxy policy command failed, traffic may NOT be routed through the proxy: `{}` ({})",
        command, detail
    );
}

/// Maintain a user-level XDG desktop override that carries the active
/// profile's proxy flags for desktop launches. The override replaces the
/// system .desktop entry while a proxied profile is active and is removed
/// again afterwards, falling back to the unmodified system entry.
#[cfg(target_os = "linux")]
fn sync_linux_desktop(browser: Browser, proxy: Option<&ProxyConfig>) -> Result<()> {
    use std::fs;
    use crate::shortcut::proxy_launch_flags;

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Ok(()),
    };
    let apps_dir = home.join(".local/share/applications");
    let user_file = apps_dir.join(browser.linux_desktop_file());

    match proxy {
        Some(p) => {
            let flags = proxy_launch_flags(Some(p));

            // Never clobber a user-maintained override that bm did not write.
            if user_file.exists() {
                let existing = fs::read_to_string(&user_file).unwrap_or_default();
                if !existing.contains(DESKTOP_OVERRIDE_MARKER) {
                    eprintln!(
                        "Warning: found user override {} without bm marker; leaving it untouched, desktop launches will NOT use the proxy.",
                        user_file.display()
                    );
                    return Ok(());
                }
            }

            // Test seam: overrides the system desktop-entry search directory.
            let system_content = std::env::var_os("BM_DESKTOP_FILE_DIR")
                .map(|dir| fs::read_to_string(std::path::PathBuf::from(dir).join(browser.linux_desktop_file())))
                .or_else(|| find_system_desktop(browser))
                .transpose()?
                .ok_or_else(|| {
                    crate::error::BmError::Other(format!(
                        "System desktop entry {} not found; cannot install proxy override",
                        browser.linux_desktop_file()
                    ))
                })?;

            fs::create_dir_all(&apps_dir)?;
            let content = format!(
                "{}\n{}",
                DESKTOP_OVERRIDE_MARKER,
                apply_proxy_to_desktop_exec(&system_content, &flags)
            );
            fs::write(&user_file, content)?;
        }
        None => {
            if user_file.exists() {
                let existing = fs::read_to_string(&user_file).unwrap_or_default();
                if existing.contains(DESKTOP_OVERRIDE_MARKER) {
                    let _ = fs::remove_file(&user_file);
                }
            }
        }
    }
    Ok(())
}

/// Locate the system .desktop entry (deb/rpm and flatpak export paths).
#[cfg(target_os = "linux")]
fn find_system_desktop(browser: Browser) -> Option<std::io::Result<String>> {
    let name = browser.linux_desktop_file();
    let candidates = [
        std::path::PathBuf::from("/usr/share/applications"),
        std::path::PathBuf::from("/usr/local/share/applications"),
        dirs::home_dir()?.join(".local/share/flatpak/exports/share/applications"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    for dir in candidates {
        let path = dir.join(name);
        if path.exists() {
            return Some(std::fs::read_to_string(path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_write_commands_with_bypass() {
        let p = ProxyConfig::parse("socks5://10.0.0.1:1080", Some("localhost;*.lan")).unwrap();
        let cmds = macos_policy_commands(Some(&p), "com.google.Chrome");
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0][..], ["write", "com.google.Chrome", "ProxyMode", "-string", "fixed_servers"]);
        assert_eq!(cmds[1][2], "ProxyServer");
        assert!(cmds[1][4].contains("socks5://10.0.0.1:1080"));
        assert_eq!(cmds[2][2], "ProxyBypassList");
        assert_eq!(cmds[2][4], "localhost,*.lan");
        assert_eq!(cmds[3][2], "WebRtcIPHandlingPolicy");
        assert_eq!(cmds[3][4], "disable_non_proxied_udp");
    }

    #[test]
    fn macos_write_commands_without_bypass_omit_key() {
        let p = ProxyConfig::parse("http://p:8080", None).unwrap();
        let cmds = macos_policy_commands(Some(&p), "com.microsoft.Edge");
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().all(|c| c[1] == "com.microsoft.Edge"));
        assert!(cmds.iter().all(|c| c[2] != "ProxyBypassList"));
    }

    #[test]
    fn macos_clear_commands_delete_all_keys() {
        let cmds = macos_policy_commands(None, "com.google.Chrome");
        assert_eq!(cmds.len(), MANAGED_POLICY_KEYS.len());
        for (cmd, key) in cmds.iter().zip(MANAGED_POLICY_KEYS.iter()) {
            assert_eq!(cmd[..], ["delete", "com.google.Chrome", *key]);
        }
    }

    /// Real end-to-end check of the defaults(1) path against a scratch
    /// domain, never a real browser policy domain.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_policy_defaults_roundtrip_on_test_domain() {
        use std::process::Command;

        fn read_key(key: &str) -> Option<String> {
            let out = Command::new("defaults")
                .args(["read", "com.bm.test.policy", key])
                .output()
                .ok()?;
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        }

        let p = ProxyConfig::parse("socks5://127.0.0.1:54321", Some("localhost")).unwrap();
        apply_policy(Browser::Chrome, Some(&p), Some("com.bm.test.policy")).unwrap();

        assert_eq!(read_key("ProxyMode").as_deref(), Some("fixed_servers"));
        assert_eq!(
            read_key("ProxyServer").as_deref(),
            Some("socks5://127.0.0.1:54321")
        );
        assert_eq!(read_key("ProxyBypassList").as_deref(), Some("localhost"));
        assert_eq!(
            read_key("WebRtcIPHandlingPolicy").as_deref(),
            Some("disable_non_proxied_udp")
        );

        apply_policy(Browser::Chrome, None, Some("com.bm.test.policy")).unwrap();
        for key in MANAGED_POLICY_KEYS {
            assert!(read_key(key).is_none(), "{key} should be removed");
        }

        let _ = Command::new("defaults").args(["delete", "com.bm.test.policy"]).status();
    }

    #[test]
    fn desktop_exec_flags_append_to_all_exec_lines() {
        let content = "[Desktop Entry]\nName=Chrome\nExec=/usr/bin/google-chrome-stable %U\n\n[Desktop Action new-window]\nExec=/usr/bin/google-chrome-stable\n";
        let out = apply_proxy_to_desktop_exec(content, " --proxy-server=\"socks5://h:1\"");
        let execs: Vec<&str> = out.lines().filter(|l| l.starts_with("Exec=")).collect();
        assert_eq!(execs.len(), 2);
        assert!(execs.iter().all(|l| l.ends_with("--proxy-server=\"socks5://h:1\"")));
        assert!(out.starts_with("[Desktop Entry]\nName=Chrome\n"));
        assert!(out.ends_with('\n'));
        // Non-Exec lines untouched
        assert!(out.contains("[Desktop Action new-window]"));
    }
}
