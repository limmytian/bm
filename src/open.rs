use std::path::Path;
use std::process::{Command, Stdio};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;
use crate::shortcut::proxy_launch_argv;
use crate::storage::StorageManager;

pub struct OpenLauncher;

/// One profile's launch result in a multi-profile run.
#[derive(Debug)]
pub struct OpenOutcome {
    pub profile: String,
    /// None on success; the error text otherwise.
    pub error: Option<String>,
}

/// Classify `bm open` tokens: the first token is always a profile name;
/// every token after it that is a *valid profile name* launches as another
/// profile, everything else opens as a URL. This keeps the historical
/// single-profile form byte-compatible (`bm open work https://x.com`,
/// `bm open work example.com` — URLs never pass name validation, profile
/// names cannot contain `: / .`) while making `bm open a b c` launch three
/// profiles. `--` stays available as clap's verbatim escape for odd tokens.
pub fn split_open_args(first: String, rest: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut profiles = vec![first];
    let mut urls = Vec::new();
    for token in rest {
        if StorageManager::validate_name(&token).is_ok() {
            profiles.push(token);
        } else {
            urls.push(token);
        }
    }
    (profiles, urls)
}

impl OpenLauncher {
    /// Launch several profiles side by side, each with its own user-data-dir
    /// and proxy flags, sharing `urls`. Every profile is attempted
    /// independently — a missing profile or failed spawn is reported in its
    /// outcome instead of aborting the rest. Single-profile launches are the
    /// one-element case: the child is spawned detached with nulled stdio so
    /// the CLI returns immediately, and a distinct user-data-dir runs side
    /// by side with any already-running browser instance (the no-ceremony
    /// counterpart to `bm shortcut`, without switching the global symlink).
    pub fn launch_all(
        storage: &StorageManager,
        browser: Browser,
        profiles: &[String],
        urls: &[String],
    ) -> Result<Vec<OpenOutcome>> {
        let binary = browser.binary_path()?;
        let mut outcomes = Vec::new();
        for profile in profiles {
            let profile_dir = storage.profile_dir(browser, profile);
            let error = if !profile_dir.exists() {
                Some(
                    BmError::ProfileNotFound {
                        name: profile.clone(),
                        browser: browser.display_name().to_string(),
                    }
                    .to_string(),
                )
            } else {
                let args = launch_args(
                    &profile_dir,
                    storage.proxy_for(browser, profile).as_ref(),
                    urls,
                );
                Command::new(&binary)
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .err()
                    .map(|e| {
                        BmError::Other(format!(
                            "Failed to launch {} ({}): {e}. Is the browser installed?",
                            browser.display_name(),
                            binary.display()
                        ))
                        .to_string()
                    })
            };
            outcomes.push(OpenOutcome {
                profile: profile.clone(),
                error,
            });
        }
        Ok(outcomes)
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
        assert!(
            args.contains(&"--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_string())
        );
        // Quoting is a shell concern; direct spawns must not carry it.
        assert!(args.iter().all(|a| !a.contains('"')));
    }

    #[test]
    fn socks5_dns_pin_stays_single_arg() {
        let p = ProxyConfig::parse("socks5://127.0.0.1:1080", None).unwrap();
        let args = launch_args(Path::new("/tmp/prof"), Some(&p), &[]);
        assert!(
            args.contains(&"--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".to_string())
        );
    }

    #[test]
    fn urls_are_appended_in_order_after_flags() {
        let urls = vec![
            "https://example.com".to_string(),
            "https://example.org".to_string(),
        ];
        let args = launch_args(Path::new("/tmp/prof"), None, &urls);
        assert_eq!(&args[..2], &["--user-data-dir=/tmp/prof", "--no-first-run"]);
        assert_eq!(&args[2..], &["https://example.com", "https://example.org"]);
    }

    #[test]
    fn multi_profile_grammar_classifies_by_name_validity() {
        // The requirement's headline syntax: bare names launch side by side.
        let (profiles, urls) = split_open_args("a".into(), vec!["b".into(), "c".into()]);
        assert_eq!(profiles, vec!["a", "b", "c"]);
        assert!(urls.is_empty());

        // Historical single-profile forms: everything that cannot be a
        // profile name stays a URL.
        let (profiles, urls) = split_open_args(
            "work".into(),
            vec!["https://x.com".into(), "example.com".into()],
        );
        assert_eq!(profiles, vec!["work"]);
        assert_eq!(urls, vec!["https://x.com", "example.com"]);

        // Mixed: names and URLs interleaved.
        let (profiles, urls) = split_open_args(
            "work".into(),
            vec![
                "https://x.com".into(),
                "personal".into(),
                "localhost:3000".into(),
            ],
        );
        assert_eq!(profiles, vec!["work", "personal"]);
        assert_eq!(urls, vec!["https://x.com", "localhost:3000"]);

        // Names with underscore/dash/digits still classify as profiles.
        let (profiles, _) =
            split_open_args("team_1".into(), vec!["old-acct".into(), "2026".into()]);
        assert_eq!(profiles, vec!["team_1", "old-acct", "2026"]);

        // Empty rest: plain single profile.
        let (profiles, urls) = split_open_args("solo".into(), vec![]);
        assert_eq!(profiles, vec!["solo"]);
        assert!(urls.is_empty());
    }
}
