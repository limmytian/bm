//! Profile-storage health audit (`bm doctor`).
//!
//! Read-only by default: `run_checks` reports every problem it can detect.
//! `--fix` applies the safe repairs only — removing a dangling default-dir
//! symlink and backfilling missing registry entries. Everything else
//! (external symlinks, orphaned metadata, broken shortcuts, macOS policy
//! conflicts, permissions) is report-only with guidance.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

use crate::browser::{Browser, ResolutionSource};
use crate::error::Result;
use crate::storage::{ProfileMetadata, StorageManager};

/// Status of browser executable detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserHealth {
    pub browser: Browser,
    pub found: bool,
    pub path: Option<PathBuf>,
    pub source: Option<ResolutionSource>,
    pub error: Option<String>,
}

/// Audit detection status for a specific browser.
pub fn check_browser_health(browser: Browser) -> BrowserHealth {
    match browser.resolve_binary_path() {
        Ok((path, source)) => BrowserHealth {
            browser,
            found: true,
            path: Some(path),
            source: Some(source),
            error: None,
        },
        Err(err) => BrowserHealth {
            browser,
            found: false,
            path: None,
            source: None,
            error: Some(err.to_string()),
        },
    }
}

/// Audit detection status for all supported browsers independently.
#[allow(dead_code)]
pub fn check_all_browsers_health() -> Vec<BrowserHealth> {
    vec![
        check_browser_health(Browser::Chrome),
        check_browser_health(Browser::Edge),
        check_browser_health(Browser::Brave),
        check_browser_health(Browser::Chromium),
    ]
}

/// A safe repair action attached to an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    /// The global default-data-dir symlink points at a missing target;
    /// removing it restores stock browser behavior.
    RemoveDanglingDefaultSymlink { path: PathBuf },
    /// A profile dir exists without a registry entry; backfill one.
    BackfillMetadata { browser: Browser, name: String },
}

/// One detected problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Check that produced the finding (stable label, e.g. "default-symlink").
    pub check: &'static str,
    /// Human-readable description.
    pub detail: String,
    /// Present when `--fix` can repair this issue automatically.
    pub repair: Option<Repair>,
}

/// Run every health check for the given browser.
pub fn run_checks(storage: &StorageManager, browser: Browser) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    check_browser_executable(browser, &mut issues)?;
    check_default_symlink(storage, browser, &mut issues)?;
    check_registry_vs_disk(storage, browser, &mut issues)?;
    issues.extend(find_broken_shortcuts(storage).into_iter().map(|(path, target)| Issue {
        check: "broken-shortcut",
        detail: format!(
            "standalone shortcut {} points at missing profile dir {} (regenerate with: bm shortcut <name>)",
            path.display(),
            target
        ),
        repair: None,
    }));
    check_policy_conflict(storage, browser, &mut issues)?;
    check_permissions(storage, &mut issues)?;
    Ok(issues)
}

/// Apply the safe repairs found in `issues`; returns how many were applied.
pub fn apply_fixes(storage: &StorageManager, issues: &[Issue]) -> Result<usize> {
    let mut fixed = 0;
    for issue in issues {
        match &issue.repair {
            Some(Repair::RemoveDanglingDefaultSymlink { path }) => {
                // Re-verify: only ever remove a symlink, never a real dir.
                if fs::symlink_metadata(path)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
                {
                    fs::remove_file(path)?;
                    fixed += 1;
                }
            }
            Some(Repair::BackfillMetadata { browser, name }) => {
                let key = format!("{browser}:{name}");
                let mut reg = storage.load_registry()?;
                match reg.profiles.entry(key) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        let now = Utc::now();
                        slot.insert(ProfileMetadata {
                            name: name.clone(),
                            browser: *browser,
                            description: String::new(),
                            created_at: now,
                            updated_at: now,
                            proxy: None,
                        });
                        storage.save_registry(&reg)?;
                        fixed += 1;
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {}
                }
            }
            None => {}
        }
    }
    Ok(fixed)
}

/// Check 0: verify that the browser executable can be located and exists.
fn check_browser_executable(browser: Browser, issues: &mut Vec<Issue>) -> Result<()> {
    let health = check_browser_health(browser);
    if !health.found {
        issues.push(Issue {
            check: "browser-executable",
            detail: format!(
                "{} executable could not be located: {}",
                browser.display_name(),
                health.error.as_deref().unwrap_or("not found")
            ),
            repair: None,
        });
    }
    Ok(())
}

/// Check 1: the browser's default-data-dir symlink.
fn check_default_symlink(
    storage: &StorageManager,
    browser: Browser,
    issues: &mut Vec<Issue>,
) -> Result<()> {
    let default_dir = storage.default_data_dir(browser)?;
    let Ok(meta) = fs::symlink_metadata(&default_dir) else {
        return Ok(()); // Absent or a real dir: nothing bm-managed to audit.
    };
    if !meta.file_type().is_symlink() {
        return Ok(());
    }

    let target = fs::read_link(&default_dir)?;
    let resolved = if target.is_relative() {
        default_dir
            .parent()
            .map(|p| p.join(&target))
            .unwrap_or_else(|| target.clone())
    } else {
        target.clone()
    };

    if !resolved.exists() {
        issues.push(Issue {
            check: "default-symlink",
            detail: format!(
                "default-data-dir symlink is dangling: {} -> {} (browser would treat it as fresh data; fix removes the symlink)",
                default_dir.display(),
                resolved.display()
            ),
            repair: Some(Repair::RemoveDanglingDefaultSymlink { path: default_dir }),
        });
    } else if !resolved.starts_with(storage.profiles_dir(browser)) {
        issues.push(Issue {
            check: "default-symlink",
            detail: format!(
                "default-data-dir symlink points outside bm profiles: {} -> {} (external activation, not repairable by bm)",
                default_dir.display(),
                resolved.display()
            ),
            repair: None,
        });
    }
    Ok(())
}

/// Check 2: registry entries without dirs, and profile dirs without entries.
fn check_registry_vs_disk(
    storage: &StorageManager,
    browser: Browser,
    issues: &mut Vec<Issue>,
) -> Result<()> {
    let reg = storage.load_registry()?;
    let prefix = format!("{browser}:");

    for (key, meta) in &reg.profiles {
        if key.starts_with(&prefix) && !storage.profile_dir(browser, &meta.name).exists() {
            issues.push(Issue {
                check: "registry-orphan",
                detail: format!(
                    "metadata entry '{}' has no profile dir on disk (re-create it with: bm create {})",
                    meta.name, meta.name
                ),
                repair: None,
            });
        }
    }

    let dir = storage.profiles_dir(browser);
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !reg.profiles.contains_key(&format!("{prefix}{name}")) {
                    issues.push(Issue {
                        check: "unregistered-dir",
                        detail: format!(
                            "profile dir '{name}' has no metadata entry (list output currently synthesizes one; fix backfills the registry)",
                        ),
                        repair: Some(Repair::BackfillMetadata { browser, name: name.to_string() }),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Standalone shortcut artifacts whose embedded `--user-data-dir` target no
/// longer exists: `(artifact path, missing target)`.
pub fn find_broken_shortcuts(storage: &StorageManager) -> Vec<(PathBuf, String)> {
    let mut broken = Vec::new();

    let scripts_dir = storage.base_dir().join("shortcuts");
    broken.extend(scan_scripts_for_broken(&scripts_dir));

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        for dir in [home.join("Applications"), home.join("Desktop")] {
            broken.extend(scan_app_bundles_for_broken(&dir));
        }
    }

    broken
}

/// Scan a dir of standalone `.sh` launchers for missing profile targets.
fn scan_scripts_for_broken(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sh") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Some(target) = embedded_user_data_dir(&content) {
                        if !Path::new(&target).exists() {
                            found.push((path, target));
                        }
                    }
                }
            }
        }
    }
    found
}

/// Scan a dir for bm-generated `.app` bundles with missing profile targets.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn scan_app_bundles_for_broken(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let launcher = path.join("Contents/MacOS/launcher");
            if let Ok(content) = fs::read_to_string(&launcher) {
                if let Some(target) = embedded_user_data_dir(&content) {
                    if !Path::new(&target).exists() {
                        found.push((path, target));
                    }
                }
            }
        }
    }
    found
}

/// Extract the path after `--user-data-dir=` (quoted or bare) from launcher
/// script content.
fn embedded_user_data_dir(content: &str) -> Option<String> {
    let idx = content.find("--user-data-dir=")?;
    let rest = &content[idx + "--user-data-dir=".len()..];
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Check 4 (macOS only): the managed-policy proxy vs the active profile's
/// proxy — the same conflict the standalone launchers warn about. Report and
/// guide, never auto-fix.
#[cfg(target_os = "macos")]
fn check_policy_conflict(
    storage: &StorageManager,
    browser: Browser,
    issues: &mut Vec<Issue>,
) -> Result<()> {
    use std::process::Command;

    let Some(active) = storage.get_current_active(browser)? else {
        return Ok(());
    };
    if active.starts_with("external (") {
        return Ok(());
    }
    let active_proxy = storage.proxy_for(browser, &active);

    let policy_server = Command::new("defaults")
        .args(["read", browser.policy_domain(), "ProxyServer"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    match (active_proxy, policy_server) {
        (Some(p), Some(policy)) if policy != p.server_string() => issues.push(Issue {
            check: "policy-mismatch",
            detail: format!(
                "managed policy ProxyServer={policy} overrides the active profile's proxy {} — side-by-side launches use the wrong egress (fix: bm proxy clear {active}, or bm switch)",
                p.server_string()
            ),
            repair: None,
        }),
        (None, Some(policy)) => issues.push(Issue {
            check: "policy-mismatch",
            detail: format!(
                "managed policy ProxyServer={policy} is set but the active profile '{active}' has no proxy (stale policy; fix: bm switch to a proxied profile or bm proxy clear <name>)"
            ),
            repair: None,
        }),
        _ => {}
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn check_policy_conflict(
    _storage: &StorageManager,
    _browser: Browser,
    _issues: &mut Vec<Issue>,
) -> Result<()> {
    Ok(())
}

/// Check 5: ~/.bm should not be readable by group/other (advisory).
#[cfg(unix)]
fn check_permissions(storage: &StorageManager, issues: &mut Vec<Issue>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if !storage.base_dir().exists() {
        return Ok(());
    }
    let mode = fs::metadata(storage.base_dir())?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        issues.push(Issue {
            check: "permissions",
            detail: format!(
                "~/.bm is mode {:o}; profiles hold sessions and cookies — consider chmod 700 ~/.bm",
                mode
            ),
            repair: None,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_storage: &StorageManager, _issues: &mut Vec<Issue>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> (tempfile::TempDir, StorageManager, Browser) {
        let tmp = tempfile::tempdir().unwrap();
        // Pre-create the base dir with the mode a real ~/.bm should have, so
        // the permissions check does not flag the test fixture itself.
        let base = tmp.path().join(".bm");
        fs::create_dir_all(&base).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let default_link = tmp.path().join("default-link");
        let storage = StorageManager::with_base_dir(base).with_default_dir_override(default_link);
        if Browser::Chrome.resolve_binary_path().is_err() {
            let mock = tmp.path().join("mock-chrome");
            let _ = fs::write(&mock, b"#!/bin/sh\n");
            std::env::set_var("BM_CHROME_BIN", &mock);
        }
        (tmp, storage, Browser::Chrome)
    }

    #[test]
    fn browser_health_checks_independent_per_browser() {
        let tmp = tempfile::tempdir().unwrap();
        let mock_brave = tmp.path().join("mock-brave");
        fs::write(&mock_brave, b"#!/bin/sh\n").unwrap();

        std::env::set_var("BM_BRAVE_BIN", &mock_brave);
        let health = check_browser_health(Browser::Brave);
        assert!(health.found);
        assert_eq!(health.path, Some(mock_brave));
        assert_eq!(
            health.source,
            Some(ResolutionSource::Env("BM_BRAVE_BIN".to_string()))
        );

        std::env::remove_var("BM_BRAVE_BIN");
    }

    #[test]
    fn check_all_browsers_health_reports_all_variants() {
        let all = check_all_browsers_health();
        assert_eq!(all.len(), 4);
        let browsers: Vec<Browser> = all.into_iter().map(|h| h.browser).collect();
        assert!(browsers.contains(&Browser::Chrome));
        assert!(browsers.contains(&Browser::Edge));
        assert!(browsers.contains(&Browser::Brave));
        assert!(browsers.contains(&Browser::Chromium));
    }

    #[test]
    fn healthy_tree_reports_nothing() {
        let (_tmp, storage, browser) = setup();
        storage.create_profile("work", None, browser).unwrap();
        let issues = run_checks(&storage, browser).unwrap();
        assert_eq!(issues, Vec::new());
    }

    #[test]
    fn dangling_default_symlink_is_detected_and_fixed() {
        let (tmp, storage, browser) = setup();
        let link = tmp.path().join("default-link");
        symlink(tmp.path().join("gone"), &link).unwrap();

        let issues = run_checks(&storage, browser).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].check, "default-symlink");
        assert!(issues[0].repair.is_some());

        assert_eq!(apply_fixes(&storage, &issues).unwrap(), 1);
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "dangling symlink removed"
        );
    }

    #[test]
    fn external_symlink_is_reported_without_fix() {
        let (tmp, storage, browser) = setup();
        let link = tmp.path().join("default-link");
        let outside = tmp.path().join("outside-data");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, &link).unwrap();

        let issues = run_checks(&storage, browser).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].check, "default-symlink");
        assert_eq!(issues[0].repair, None, "external symlink is not fixable");
    }

    #[test]
    fn unregistered_dir_backfills_and_orphan_metadata_reports() {
        let (_tmp, storage, browser) = setup();

        // Dir on disk without registry entry.
        let orphan = storage.profile_dir(browser, "orphandir");
        fs::create_dir_all(&orphan).unwrap();

        // Registry entry without dir.
        let mut reg = storage.load_registry().unwrap();
        let now = Utc::now();
        reg.profiles.insert(
            "chrome:ghost".into(),
            ProfileMetadata {
                name: "ghost".into(),
                browser,
                description: String::new(),
                created_at: now,
                updated_at: now,
                proxy: None,
            },
        );
        storage.save_registry(&reg).unwrap();

        let issues = run_checks(&storage, browser).unwrap();
        assert_eq!(issues.len(), 2);
        let backfill = issues
            .iter()
            .find(|i| i.check == "unregistered-dir")
            .expect("unregistered-dir issue");
        let orphan_meta = issues
            .iter()
            .find(|i| i.check == "registry-orphan")
            .expect("registry-orphan issue");
        assert_eq!(orphan_meta.repair, None);

        assert_eq!(
            apply_fixes(&storage, std::slice::from_ref(backfill)).unwrap(),
            1
        );
        let reg = storage.load_registry().unwrap();
        assert!(reg.profiles.contains_key("chrome:orphandir"));
    }

    #[test]
    fn broken_shortcuts_are_detected() {
        let (_tmp, storage, browser) = setup();
        let missing = storage.profile_dir(browser, "deleted-profile");

        let scripts = storage.base_dir().join("shortcuts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(
            scripts.join("chrome-gone.sh"),
            format!(
                "#!/bin/sh\nexec \"chrome\" --user-data-dir=\"{}\" --no-first-run \"$@\"\n",
                missing.display()
            ),
        )
        .unwrap();
        // A healthy artifact must not be reported.
        let healthy_dir = storage.profile_dir(browser, "alive");
        fs::create_dir_all(&healthy_dir).unwrap();
        fs::write(
            scripts.join("chrome-alive.sh"),
            format!(
                "#!/bin/sh\nexec \"chrome\" --user-data-dir=\"{}\" --no-first-run \"$@\"\n",
                healthy_dir.display()
            ),
        )
        .unwrap();

        let broken = find_broken_shortcuts(&storage);
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].1, missing.to_string_lossy().to_string());
    }

    #[test]
    fn embedded_user_data_dir_parses_quoted_and_bare() {
        assert_eq!(
            embedded_user_data_dir("exec \"bin\" --user-data-dir=\"/tmp/p\" --no-first-run"),
            Some("/tmp/p".to_string())
        );
        assert_eq!(
            embedded_user_data_dir("exec bin --user-data-dir=/tmp/p2 --no-first-run"),
            Some("/tmp/p2".to_string())
        );
        assert_eq!(embedded_user_data_dir("exec bin"), None);
    }

    #[test]
    fn permissive_base_dir_is_flagged() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, storage, browser) = setup();
        storage.create_profile("work", None, browser).unwrap();
        fs::set_permissions(storage.base_dir(), fs::Permissions::from_mode(0o755)).unwrap();

        let issues = run_checks(&storage, browser).unwrap();
        assert!(issues.iter().any(|i| i.check == "permissions"));
    }
}
