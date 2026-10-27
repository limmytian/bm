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
    /// Live CDP endpoint (ws://127.0.0.1:…) when `--cdp` was requested.
    pub cdp_endpoint: Option<String>,
    /// True when the CDP endpoint belonged to an already-running instance
    /// (singleton handoff avoided a duplicate spawn).
    pub cdp_reused: bool,
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
    ///
    /// With `cdp`, each launch adds `--remote-debugging-port=0` (OS-assigned
    /// port, loopback binding) and the real endpoint is discovered from
    /// `DevToolsActivePort` in the profile dir, then persisted to
    /// ~/.bm/cdp.json (0600). An already-running instance is never spawned
    /// twice: if it already serves CDP, its live endpoint is returned; if it
    /// does not, launching cannot enable debugging (the singleton handoff
    /// ignores new flags) and the outcome says so.
    pub fn launch_all(
        storage: &StorageManager,
        browser: Browser,
        profiles: &[String],
        urls: &[String],
        cdp: bool,
    ) -> Result<Vec<OpenOutcome>> {
        let binary = browser.binary_path()?;
        let mut outcomes = Vec::new();
        for profile in profiles {
            let profile_dir = storage.profile_dir(browser, profile);
            if !profile_dir.exists() {
                outcomes.push(OpenOutcome {
                    profile: profile.clone(),
                    error: Some(
                        BmError::ProfileNotFound {
                            name: profile.clone(),
                            browser: browser.display_name().to_string(),
                        }
                        .to_string(),
                    ),
                    cdp_endpoint: None,
                    cdp_reused: false,
                });
                continue;
            }

            if cdp {
                // Already running? A live instance either serves CDP (adopt
                // its endpoint) or cannot gain it from a re-launch.
                if let Some(pid) = crate::running::running_pid(&profile_dir)? {
                    if let Some(entry) = adopt_running_endpoint(&profile_dir, Some(pid)) {
                        let url = entry.ws_url();
                        persist_endpoint(storage, browser, profile, entry);
                        outcomes.push(OpenOutcome {
                            profile: profile.clone(),
                            error: None,
                            cdp_endpoint: Some(url),
                            cdp_reused: true,
                        });
                        continue;
                    }
                    outcomes.push(OpenOutcome {
                        profile: profile.clone(),
                        error: Some(format!(
                            "profile is already running (pid {pid}) without --cdp — close it and relaunch; a re-launch cannot enable debugging on a running instance"
                        )),
                        cdp_endpoint: None,
                        cdp_reused: false,
                    });
                    continue;
                }

                let mut args = launch_args(
                    &profile_dir,
                    storage.proxy_for(browser, profile).as_ref(),
                    urls,
                );
                args.extend(crate::cdp::cdp_launch_flags());
                let spawned = Command::new(&binary)
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
                let spawned_pid = spawned.id();

                match crate::cdp::wait_for_endpoint(&profile_dir) {
                    Ok(mut entry) => {
                        entry.pid = Some(spawned_pid);
                        let url = entry.ws_url();
                        persist_endpoint(storage, browser, profile, entry);
                        outcomes.push(OpenOutcome {
                            profile: profile.clone(),
                            error: None,
                            cdp_endpoint: Some(url),
                            cdp_reused: false,
                        });
                    }
                    Err(e) => outcomes.push(OpenOutcome {
                        profile: profile.clone(),
                        error: Some(e.to_string()),
                        cdp_endpoint: None,
                        cdp_reused: false,
                    }),
                }
                continue;
            }

            let args = launch_args(
                &profile_dir,
                storage.proxy_for(browser, profile).as_ref(),
                urls,
            );
            let error = Command::new(&binary)
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
                });
            outcomes.push(OpenOutcome {
                profile: profile.clone(),
                error,
                cdp_endpoint: None,
                cdp_reused: false,
            });
        }
        Ok(outcomes)
    }
}

/// Read and TCP-verify the DevToolsActivePort of a running instance, if it
/// actually serves CDP right now. Returns None when the file is missing,
/// stale, or the port refuses connections.
fn adopt_running_endpoint(profile_dir: &Path, pid: Option<u32>) -> Option<crate::cdp::CdpEntry> {
    let content = std::fs::read_to_string(profile_dir.join("DevToolsActivePort")).ok()?;
    let (port, ws_path) = crate::cdp::parse_port_file(&content)?;
    if !crate::cdp::port_alive(port) {
        return None;
    }
    Some(crate::cdp::CdpEntry {
        port,
        ws_path,
        pid,
        started_at: chrono::Utc::now(),
    })
}

/// Persist an adopted or freshly discovered endpoint in the registry. The
/// launch itself succeeded, so a registry hiccup must not fail the command.
fn persist_endpoint(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    entry: crate::cdp::CdpEntry,
) {
    let mut registry = crate::cdp::load_registry(storage);
    registry.insert(format!("{browser}:{profile}"), entry);
    if let Err(e) = crate::cdp::save_registry(storage, &registry) {
        eprintln!("warning: could not persist the CDP endpoint registry: {e}");
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

    #[test]
    fn adopt_running_endpoint_verifies_the_port() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // No file → None.
        assert!(adopt_running_endpoint(dir, Some(1)).is_none());

        // Live listener: file + real socket → adopted.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/t\n"),
        )
        .unwrap();
        let entry = adopt_running_endpoint(dir, Some(99)).unwrap();
        assert_eq!(entry.port, port);
        assert_eq!(entry.pid, Some(99));
        assert_eq!(
            entry.ws_url(),
            format!("ws://127.0.0.1:{port}/devtools/browser/t")
        );

        // Same file content but nothing listening (stale) → None.
        drop(listener);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            adopt_running_endpoint(dir, Some(99)).is_none(),
            "stale port must not be adopted"
        );
    }
}
