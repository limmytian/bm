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
        let proxy = storage
            .list_profiles(browser)
            .ok()
            .and_then(|profiles| {
                profiles
                    .into_iter()
                    .find(|(m, _)| m.name == profile_name)
                    .and_then(|(m, _)| m.proxy)
            });

        if cfg!(target_os = "macos") {
            Self::create_macos_app_bundle(browser, profile_name, &profile_dir, proxy.as_ref(), dest_dir)
        } else {
            Self::create_linux_script(storage, browser, profile_name, &profile_dir, proxy.as_ref(), dest_dir)
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
        let home = dirs::home_dir().ok_or_else(|| {
            BmError::Other("Unable to resolve home directory".into())
        })?;

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
                let desktop_file = apps_dir.join(format!("bm-{}-{}.desktop", browser, profile_name));
                let display_browser = browser.display_name();
                let icon_name = match browser {
                    Browser::Chrome => "google-chrome",
                    Browser::Edge => "microsoft-edge",
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
}
