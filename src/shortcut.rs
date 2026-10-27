use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::{ProxyConfig, ProxyScheme};
use crate::storage::StorageManager;

/// Chromium launch flags enforcing the profile's egress proxy: the proxy
/// server itself, optional bypass rules, and WebRTC leak protection
/// (non-proxied UDP disabled). SOCKS5 additionally pins hostname resolution
/// to the proxy so DNS never leaks to the local resolver.
///
/// The returned string starts with a leading space (or is empty) so it can
/// be interpolated directly after other flags. Shared by standalone
/// shortcuts and the Linux desktop-override policy sync.
pub(crate) fn proxy_launch_flags(proxy: Option<&ProxyConfig>) -> String {
    let Some(p) = proxy else {
        return String::new();
    };

    let mut flags = format!(" --proxy-server=\"{}\"", p.server_string());
    if let Some(bypass) = p.bypass_string() {
        flags.push_str(&format!(" --proxy-bypass-list=\"{}\"", bypass));
    }
    flags.push_str(" --force-webrtc-ip-handling-policy=disable_non_proxied_udp");
    if p.scheme == ProxyScheme::Socks5 {
        let host = if p.host.contains(':') {
            format!("[{}]", p.host)
        } else {
            p.host.clone()
        };
        flags.push_str(&format!(
            " --host-resolver-rules=\"MAP * ~NOTFOUND , EXCLUDE {}\"",
            host
        ));
    }
    flags
}

/// Argv form of `proxy_launch_flags` for direct `Command` spawning: splits on
/// unquoted spaces and strips the double quotes that exist only for shell
/// interpolation. Quoted values may contain spaces (the SOCKS5 DNS pin does)
/// and must stay a single argv entry.
pub(crate) fn proxy_launch_argv(proxy: Option<&ProxyConfig>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in proxy_launch_flags(proxy).chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' if !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Pre-launch guard embedded in macOS launchers: a managed policy
/// (written by bm for the globally active profile) overrides
/// `--proxy-server` flags, so warn when this launcher's own proxy differs
/// from the currently active policy. Empty when no proxy is configured.
fn macos_policy_conflict_check(browser: Browser, proxy: Option<&ProxyConfig>) -> String {
    let Some(p) = proxy else {
        return String::new();
    };
    let own = p.server_string();
    format!(
        "# bm: macOS managed policy overrides --proxy-server flags; warn on mismatch.\n\
         ACTIVE_POLICY=$(defaults read {domain} ProxyServer 2>/dev/null || true)\n\
         if [ -n \"$ACTIVE_POLICY\" ] && [ \"$ACTIVE_POLICY\" != '{own}' ]; then\n\
         \x20 echo \"WARNING: this launcher's proxy {own} is overridden by managed policy ProxyServer=$ACTIVE_POLICY (bm active profile). This window may use the wrong egress; fix with 'bm proxy clear <active-profile>' or 'bm switch'.\" >&2\n\
         fi\n",
        domain = browser.policy_domain(),
        own = own
    )
}

pub struct ShortcutGenerator;

impl ShortcutGenerator {
    /// Generate a standalone desktop shortcut (.app on macOS, shell script on Linux).
    pub fn create_shortcut(
        storage: &StorageManager,
        browser: Browser,
        profile_name: &str,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let profile_dir = storage.profile_dir(browser, profile_name);
        if !profile_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: profile_name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        // Proxy config lives in profile metadata; profiles that exist only as
        // folders (no metadata) simply launch without a proxy.
        let proxy = storage.proxy_for(browser, profile_name);

        if cfg!(target_os = "macos") {
            Self::create_macos_app_bundle(
                browser,
                profile_name,
                &profile_dir,
                proxy.as_ref(),
                dest_dir,
            )
        } else {
            Self::create_linux_script(
                storage,
                browser,
                profile_name,
                &profile_dir,
                proxy.as_ref(),
                dest_dir,
            )
        }
    }

    /// Create macOS standalone .app bundle.
    fn create_macos_app_bundle(
        browser: Browser,
        profile_name: &str,
        profile_dir: &Path,
        proxy: Option<&ProxyConfig>,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| BmError::Other("Unable to resolve home directory".into()))?;

        let target_parent = match dest_dir {
            Some(p) => p.to_path_buf(),
            None => {
                let user_apps = home.join("Applications");
                if user_apps.exists() {
                    user_apps
                } else {
                    home.join("Desktop")
                }
            }
        };

        if !target_parent.exists() {
            fs::create_dir_all(&target_parent)?;
        }

        let prefix = match browser {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
            Browser::Brave => "Brave",
            Browser::Chromium => "Chromium",
        };

        let app_name = format!("{} ({}).app", prefix, profile_name);
        let app_dir = target_parent.join(&app_name);

        if app_dir.exists() {
            fs::remove_dir_all(&app_dir)?;
        }

        let macos_dir = app_dir.join("Contents/MacOS");
        let resources_dir = app_dir.join("Contents/Resources");
        fs::create_dir_all(&macos_dir)?;
        fs::create_dir_all(&resources_dir)?;

        let binary_path = browser.binary_path()?;
        let launcher_path = macos_dir.join("launcher");

        let script = format!(
            "#!/bin/sh\n{}exec \"{}\" --user-data-dir=\"{}\" --no-first-run{} \"$@\"\n",
            macos_policy_conflict_check(browser, proxy),
            binary_path.display(),
            profile_dir.display(),
            proxy_launch_flags(proxy)
        );

        fs::write(&launcher_path, script)?;
        let mut perms = fs::metadata(&launcher_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&launcher_path, perms)?;

        // Try to copy Info.plist and app icon from original application bundle
        if let Ok(orig_app) = browser.app_path() {
            let orig_plist = orig_app.join("Contents/Info.plist");
            let target_plist = app_dir.join("Contents/Info.plist");
            if orig_plist.exists() {
                let _ = fs::copy(&orig_plist, &target_plist);
                let _ = Command::new("/usr/libexec/PlistBuddy")
                    .args([
                        "-c",
                        "Set :CFBundleExecutable launcher",
                        target_plist.to_str().unwrap_or(""),
                    ])
                    .status();
                let _ = Command::new("/usr/libexec/PlistBuddy")
                    .args([
                        "-c",
                        &format!("Set :CFBundleName {} ({})", prefix, profile_name),
                        target_plist.to_str().unwrap_or(""),
                    ])
                    .status();
            }

            // Copy icon
            let icon_names = ["app.icns", "chrome.icns", "AppIcon.icns"];
            for icon_name in &icon_names {
                let orig_icon = orig_app.join("Contents/Resources").join(icon_name);
                if orig_icon.exists() {
                    let _ = fs::copy(&orig_icon, resources_dir.join("app.icns"));
                    break;
                }
            }
        }

        Ok(app_dir)
    }

    /// Create standalone launcher shell script for Linux.
    fn create_linux_script(
        storage: &StorageManager,
        browser: Browser,
        profile_name: &str,
        profile_dir: &Path,
        proxy: Option<&ProxyConfig>,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let shortcuts_dir = match dest_dir {
            Some(p) => p.to_path_buf(),
            None => storage.base_dir().join("shortcuts"),
        };
        fs::create_dir_all(&shortcuts_dir)?;

        let script_file = shortcuts_dir.join(format!("{}-{}.sh", browser, profile_name));
        let binary_path = browser.binary_path()?;

        let script = format!(
            "#!/bin/sh\nexec \"{}\" --user-data-dir=\"{}\" --no-first-run{} \"$@\"\n",
            binary_path.display(),
            profile_dir.display(),
            proxy_launch_flags(proxy)
        );

        fs::write(&script_file, script)?;
        let mut perms = fs::metadata(&script_file)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_file, perms)?;

        // Also create an XDG .desktop entry for Linux desktop environments (GNOME, KDE, etc.)
        if let Some(home) = dirs::home_dir() {
            let apps_dir = home.join(".local/share/applications");
            if fs::create_dir_all(&apps_dir).is_ok() {
                let desktop_file =
                    apps_dir.join(format!("bm-{}-{}.desktop", browser, profile_name));
                let display_browser = browser.display_name();
                let icon_name = match browser {
                    Browser::Chrome => "google-chrome",
                    Browser::Edge => "microsoft-edge",
                    Browser::Brave => "brave-browser",
                    Browser::Chromium => "chromium",
                };
                let desktop_content = format!(
                    "[Desktop Entry]\n\
Version=1.0\n\
Type=Application\n\
Name={display_browser} ({profile_name})\n\
Comment=Launch {display_browser} profile {profile_name}\n\
Exec=\"{}\" %U\n\
Icon={icon_name}\n\
Terminal=false\n\
Categories=Network;WebBrowser;\n",
                    script_file.display()
                );
                let _ = fs::write(desktop_file, desktop_content);
            }
        }

        Ok(script_file)
    }

    /// Find managed launcher artifacts that still reference the profile dir
    /// of `old_name` — they break once the profile is renamed. Checks the bm
    /// shortcuts dir and, on macOS, the default .app destinations
    /// (~/Applications, ~/Desktop).
    pub fn find_stale_shortcuts(
        storage: &StorageManager,
        browser: Browser,
        old_name: &str,
    ) -> Vec<PathBuf> {
        let old_dir = storage.profile_dir(browser, old_name);
        let old_dir_str = old_dir.to_string_lossy().to_string();
        let mut stale = Vec::new();

        let scripts_dir = storage.base_dir().join("shortcuts");
        if let Ok(entries) = fs::read_dir(&scripts_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sh") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if content.contains(&old_dir_str) {
                            stale.push(path);
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Some(home) = dirs::home_dir() {
                for dir in [home.join("Applications"), home.join("Desktop")] {
                    stale.extend(find_stale_app_bundles(browser, &old_dir, &dir));
                }
            }
        }

        stale
    }

    /// Regenerate stale launcher artifacts for the renamed profile at their
    /// original locations and remove the outdated ones (including the Linux
    /// XDG .desktop entry of the old name, best-effort).
    pub fn regenerate_stale_shortcuts(
        storage: &StorageManager,
        browser: Browser,
        stale: &[PathBuf],
        new_name: &str,
    ) -> Result<Vec<PathBuf>> {
        let mut regenerated = Vec::new();
        for old_path in stale {
            let dest = old_path.parent().map(|p| p.to_path_buf());
            let new_path = Self::create_shortcut(storage, browser, new_name, dest.as_deref())?;

            if old_path.is_dir() {
                fs::remove_dir_all(old_path)?;
            } else {
                let _ = fs::remove_file(old_path);
            }
            #[cfg(target_os = "linux")]
            if let Some(file_name) = old_path.file_name().and_then(|n| n.to_str()) {
                // "{browser}-{name}.sh" → "bm-{browser}-{name}.desktop"
                let stem = file_name.strip_suffix(".sh").unwrap_or(file_name);
                if let Some(home) = dirs::home_dir() {
                    let desktop = home
                        .join(".local/share/applications")
                        .join(format!("bm-{stem}.desktop"));
                    let _ = fs::remove_file(desktop);
                }
            }
            regenerated.push(new_path);
        }
        Ok(regenerated)
    }
}

/// Scan `dir` for bm-generated .app bundles whose launcher embeds `old_dir`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn find_stale_app_bundles(browser: Browser, old_dir: &Path, dir: &Path) -> Vec<PathBuf> {
    let prefix = match browser {
        Browser::Chrome => "Chrome",
        Browser::Edge => "Edge",
        Browser::Brave => "Brave",
        Browser::Chromium => "Chromium",
    };
    let old_dir_str = old_dir.to_string_lossy().to_string();
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_app = path.extension().and_then(|e| e.to_str()) == Some("app");
            let name_ok = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix))
                .unwrap_or(false);
            if !(is_app && name_ok) {
                continue;
            }
            let launcher = path.join("Contents/MacOS/launcher");
            if let Ok(content) = fs::read_to_string(&launcher) {
                if content.contains(&old_dir_str) {
                    found.push(path);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy_yields_no_flags() {
        assert_eq!(proxy_launch_flags(None), "");
    }

    #[test]
    fn http_proxy_flags() {
        let p = ProxyConfig::parse("http://proxy.corp:8080", Some("localhost;*.lan")).unwrap();
        let flags = proxy_launch_flags(Some(&p));
        assert!(flags.contains("--proxy-server=\"http://proxy.corp:8080\""));
        assert!(flags.contains("--proxy-bypass-list=\"localhost,*.lan\""));
        assert!(flags.contains("--force-webrtc-ip-handling-policy=disable_non_proxied_udp"));
        // DNS pinning is SOCKS5-only
        assert!(!flags.contains("--host-resolver-rules"));
    }

    #[test]
    fn socks5_proxy_pins_dns() {
        let p = ProxyConfig::parse("socks5://127.0.0.1:1080", None).unwrap();
        let flags = proxy_launch_flags(Some(&p));
        assert!(flags.contains("--proxy-server=\"socks5://127.0.0.1:1080\""));
        assert!(flags.contains("--host-resolver-rules=\"MAP * ~NOTFOUND , EXCLUDE 127.0.0.1\""));
        // No bypass configured -> no bypass flag
        assert!(!flags.contains("--proxy-bypass-list"));
    }

    #[test]
    fn socks5_ipv6_proxy_excludes_bracketed_host() {
        let p = ProxyConfig::parse("socks5://[::1]:1080", None).unwrap();
        let flags = proxy_launch_flags(Some(&p));
        assert!(flags.contains("EXCLUDE [::1]"));
    }

    #[test]
    fn argv_form_splits_flags_and_preserves_quoted_spaces() {
        assert!(proxy_launch_argv(None).is_empty());

        let p = ProxyConfig::parse("socks5://[::1]:1080", None).unwrap();
        let argv = proxy_launch_argv(Some(&p));
        assert!(argv.contains(&"--proxy-server=socks5://[::1]:1080".to_string()));
        // The DNS-pin value contains spaces and must stay a single argv entry.
        assert_eq!(
            argv.last().unwrap(),
            "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE [::1]"
        );
        // Quoting is a shell concern; argv entries must not carry it.
        assert!(argv.iter().all(|a| !a.contains('"')));
    }

    #[test]
    fn stale_shortcut_detection_and_regeneration() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
        let browser = Browser::Chrome;
        storage.create_profile("old", None, browser).unwrap();
        let old_dir = storage.profile_dir(browser, "old");

        // Fabricate a standalone script referencing the old profile dir, and
        // an unrelated one that must not match.
        let shortcuts_dir = storage.base_dir().join("shortcuts");
        fs::create_dir_all(&shortcuts_dir).unwrap();
        let script = shortcuts_dir.join("chrome-old.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nexec \"chrome\" --user-data-dir=\"{}\" --no-first-run \"$@\"\n",
                old_dir.display()
            ),
        )
        .unwrap();
        fs::write(
            shortcuts_dir.join("chrome-other.sh"),
            "#!/bin/sh\nexec x --user-data-dir=\"/elsewhere\"\n",
        )
        .unwrap();

        let stale = ShortcutGenerator::find_stale_shortcuts(&storage, browser, "old");
        assert_eq!(stale, vec![script.clone()]);

        // Fabricated .app bundle detection (explicit scan dir, no real home).
        let apps = tmp.path().join("Apps");
        let bundle = apps.join("Chrome (old).app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(
            bundle.join("Contents/MacOS/launcher"),
            format!("exec \"{}\"", old_dir.display()),
        )
        .unwrap();
        let stale_apps = find_stale_app_bundles(browser, &old_dir, &apps);
        assert_eq!(stale_apps, vec![bundle.clone()]);
        // A non-bm bundle (wrong prefix) is ignored.
        let foreign = apps.join("Other.app");
        fs::create_dir_all(foreign.join("Contents/MacOS")).unwrap();
        fs::write(
            foreign.join("Contents/MacOS/launcher"),
            format!("exec \"{}\"", old_dir.display()),
        )
        .unwrap();
        assert_eq!(find_stale_app_bundles(browser, &old_dir, &apps).len(), 1);

        // Rename, then regenerate in place: new artifact written next to the
        // old one, outdated artifact removed.
        storage
            .rename_profile_unchecked("old", "new", browser)
            .unwrap();
        let regen = ShortcutGenerator::regenerate_stale_shortcuts(&storage, browser, &stale, "new")
            .unwrap();
        assert_eq!(regen.len(), 1);
        assert!(!script.exists());
        assert!(regen[0].exists());
        // The regenerated artifact is a shell script (Linux) or an .app
        // bundle whose launcher holds the flags (macOS).
        let artifact = &regen[0];
        let content = if artifact.is_dir() {
            fs::read_to_string(artifact.join("Contents/MacOS/launcher")).unwrap()
        } else {
            fs::read_to_string(artifact).unwrap()
        };
        assert!(content.contains(
            &storage
                .profile_dir(browser, "new")
                .to_string_lossy()
                .to_string()
        ));
    }
}
