use std::path::Path;
use std::process::{Command, Stdio};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;
use crate::shortcut::proxy_launch_argv;
use crate::storage::StorageManager;

pub struct OpenLauncher;

impl OpenLauncher {
    /// Launch the browser directly against a profile dir, without switching
    /// the global default-data-dir symlink and without creating any .app or
    /// .desktop artifact — the no-ceremony counterpart to `bm shortcut`.
    /// The child is spawned detached with nulled stdio so the CLI returns
    /// immediately; a distinct user-data-dir runs side by side with any
    /// already-running browser instance.
    pub fn launch(
        storage: &StorageManager,
        browser: Browser,
        profile_name: &str,
        urls: &[String],
    ) -> Result<()> {
        let profile_dir = storage.profile_dir(browser, profile_name);
        if !profile_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: profile_name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        let binary = browser.binary_path()?;
        let args = launch_args(
            &profile_dir,
            storage.proxy_for(browser, profile_name).as_ref(),
            urls,
        );

        Command::new(&binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                BmError::Other(format!(
                    "Failed to launch {} ({}): {e}. Is the browser installed?",
                    browser.display_name(),
                    binary.display()
                ))
            })?;

        Ok(())
    }
}

/// Assemble the argv for an ad-hoc launch: pin the profile directory,
/// suppress first-run wizards, apply the profile's proxy flags, then hand
/// over any URLs to open.
pub(crate) fn launch_args(
    profile_dir: &Path,
    proxy: Option<&ProxyConfig>,
    urls: &[String],
) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".to_string(),
    ];
    args.extend(proxy_launch_argv(proxy));
    args.extend(urls.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_without_proxy_or_urls() {
        let args = launch_args(Path::new("/tmp/prof"), None, &[]);
        assert_eq!(args, vec!["--user-data-dir=/tmp/prof", "--no-first-run"]);
    }

    #[test]
    fn proxy_flags_become_bare_argv_entries() {
        let p = ProxyConfig::parse("http://proxy.corp:8080", Some("localhost;*.lan")).unwrap();
        let args = launch_args(Path::new("/tmp/prof"), Some(&p), &[]);
        assert!(args.contains(&"--proxy-server=http://proxy.corp:8080".to_string()));
        assert!(args.contains(&"--proxy-bypass-list=localhost,*.lan".to_string()));
        assert!(args.contains(
            &"--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_string()
        ));
        // Quoting is a shell concern; direct spawns must not carry it.
        assert!(args.iter().all(|a| !a.contains('"')));
    }

    #[test]
    fn socks5_dns_pin_stays_single_arg() {
        let p = ProxyConfig::parse("socks5://127.0.0.1:1080", None).unwrap();
        let args = launch_args(Path::new("/tmp/prof"), Some(&p), &[]);
        assert!(args.contains(
            &"--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".to_string()
        ));
    }

    #[test]
    fn urls_are_appended_in_order_after_flags() {
        let urls = vec![
            "https://example.com".to_string(),
            "https://example.org".to_string(),
        ];
        let args = launch_args(Path::new("/tmp/prof"), None, &urls);
        assert_eq!(&args[..2], &["--user-data-dir=/tmp/prof", "--no-first-run"]);
        assert_eq!(
            &args[2..],
            &["https://example.com", "https://example.org"]
        );
    }
}
