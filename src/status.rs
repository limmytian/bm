//! `bm status`: the one-screen, read-only overview of what bm is doing for
//! the selected browser — active profile, which profiles have a live browser
//! process (and its pid), per-profile proxy state, the newest backup of each
//! profile, and open doctor findings. No browser launch, no writes; fast
//! enough to alias.
//!
//! Everything is collected into [`StatusReport`] first and rendered second,
//! so the scripted `--json` form (scripting requirement) reuses the exact
//! same collection path as the human table.

use std::path::Path;

use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::Serialize;

use crate::backup::latest_backup;
use crate::browser::Browser;
use crate::error::Result;
use crate::storage::StorageManager;

/// Status of one profile.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileStatus {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub active: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_pid: Option<u32>,
    /// Egress proxy as `scheme://host:port`, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_backup: Option<LatestBackupInfo>,
}

/// JSON-friendly mirror of [`LatestBackup`].
#[derive(Debug, Clone, Serialize)]
pub struct LatestBackupInfo {
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub bytes: u64,
    pub encrypted: bool,
}

/// One doctor finding as status shows it: severity = auto-fixable or not.
#[derive(Debug, Clone, Serialize)]
pub struct StatusIssue {
    pub check: &'static str,
    pub detail: String,
    pub auto_fixable: bool,
}

/// Full report for one browser.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub browser: Browser,
    /// The active profile name, or the `external (…)` marker for foreign
    /// activations, or None when the browser runs on unmanaged data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    pub profiles: Vec<ProfileStatus>,
    pub doctor: DoctorSummary,
}

/// Open doctor findings, condensed for the overview.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    pub issues: Vec<StatusIssue>,
    pub auto_fixable: usize,
}

/// Collect the whole report. Read-only; never launches a browser.
pub fn collect_status(storage: &StorageManager, browser: Browser) -> Result<StatusReport> {
    let active_profile = storage.get_current_active(browser)?;
    let backups_dir = storage.base_dir().join("backups");

    let mut profiles = Vec::new();
    for (meta, is_active) in storage.list_profiles(browser)? {
        let dir = storage.profile_dir(browser, &meta.name);
        let running_pid = crate::running::running_pid(&dir)?;
        let latest = if backups_dir.exists() {
            latest_backup(&backups_dir, browser, &meta.name)?
        } else {
            None
        };
        profiles.push(ProfileStatus {
            name: meta.name,
            description: meta.description,
            active: is_active,
            running: running_pid.is_some(),
            running_pid,
            proxy: meta.proxy.map(|p| p.to_string()),
            latest_backup: latest.map(|b| LatestBackupInfo {
                path: b.path.display().to_string(),
                created_at: b.created_at,
                bytes: b.bytes,
                encrypted: b.encrypted,
            }),
        });
    }

    // Doctor findings: the same checks `bm doctor` runs, reported as data.
    let issues = crate::doctor::run_checks(storage, browser)?
        .into_iter()
        .map(|issue| StatusIssue {
            check: issue.check,
            detail: issue.detail,
            auto_fixable: issue.repair.is_some(),
        })
        .collect::<Vec<_>>();
    let auto_fixable = issues.iter().filter(|i| i.auto_fixable).count();

    Ok(StatusReport {
        browser,
        active_profile,
        profiles,
        doctor: DoctorSummary {
            issues,
            auto_fixable,
        },
    })
}

/// Human one-screen rendering of the report.
pub fn render_status(report: &StatusReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(
        out,
        "{} for {}",
        "bm status".bold(),
        report.browser.display_name().cyan().bold()
    );

    match &report.active_profile {
        Some(active) if !active.starts_with("external (") => {
            let _ = writeln!(out, "  Active profile: {}", active.green().bold());
        }
        Some(external) => {
            let _ = writeln!(
                out,
                "  Active profile: {} — the default data dir is not bm-managed",
                external.yellow()
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  Active profile: {} (browser uses its stock, unmanaged data)",
                "none".yellow()
            );
        }
    }

    let running = report.profiles.iter().filter(|p| p.running).count();
    let _ = writeln!(
        out,
        "  Profiles: {} ({} running) · Doctor: {}",
        report.profiles.len(),
        running,
        render_doctor_hint(&report.doctor),
    );

    if report.profiles.is_empty() {
        let _ = writeln!(
            out,
            "  {} No profiles yet — create one with 'bm create <name>'.",
            "-".dimmed()
        );
    } else {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  {:<3} {:<18} {:<10} {:<28} {}",
            "ACT",
            "NAME".bold(),
            "STATE".bold(),
            "PROXY".bold(),
            "LATEST BACKUP".bold()
        );
        let _ = writeln!(out, "  {}", "-".repeat(92).dimmed());
        for p in &report.profiles {
            let act = if p.active {
                "*".green().bold().to_string()
            } else {
                " ".into()
            };
            let name = if p.active {
                p.name.green().bold().to_string()
            } else {
                p.name.clone()
            };
            let state = if p.running {
                match p.running_pid {
                    Some(pid) => format!("run {}", pid),
                    None => "run".to_string(),
                }
                .cyan()
            } else {
                "idle".dimmed()
            };
            let proxy = match &p.proxy {
                Some(proxy) => truncate(proxy, 26).cyan(),
                None => "-".dimmed(),
            };
            let backup = match &p.latest_backup {
                Some(b) => {
                    let marker = if b.encrypted { " age" } else { "" };
                    format!(
                        "{} ({},{}{})",
                        b.created_at.format("%Y-%m-%d %H:%M"),
                        crate::size::format_bytes(b.bytes),
                        path_file_name(&b.path).unwrap_or_else(|| "?".into()),
                        marker
                    )
                }
                None => "-".dimmed().to_string(),
            };
            let _ = writeln!(
                out,
                "  {:<3} {:<18} {:<10} {:<28} {}",
                act, name, state, proxy, backup
            );
        }
    }

    if !report.doctor.issues.is_empty() {
        let _ = writeln!(out);
        for issue in &report.doctor.issues {
            let severity = if issue.auto_fixable {
                "fixable".cyan()
            } else {
                "report-only".dimmed()
            };
            let _ = writeln!(
                out,
                "  {} [{}] {} ({})",
                "!".yellow().bold(),
                issue.check,
                issue.detail,
                severity
            );
        }
        let _ = writeln!(
            out,
            "  Run {} for details{}.",
            "bm doctor".green(),
            if report.doctor.auto_fixable > 0 {
                " (some are auto-fixable with --fix)"
            } else {
                ""
            }
        );
    }

    out
}

/// Truncate a string to `max` characters (char-boundary safe) with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(2)).collect();
        format!("{}...", kept)
    }
}

fn render_doctor_hint(doctor: &DoctorSummary) -> String {
    if doctor.issues.is_empty() {
        "ok".green().to_string()
    } else {
        format!(
            "{} finding(s), {} auto-fixable",
            doctor.issues.len(),
            doctor.auto_fixable
        )
        .yellow()
        .to_string()
    }
}

fn path_file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        std::fs::create_dir_all(&base).unwrap();
        // The mode a real ~/.bm should have, so the doctor permissions
        // check does not flag the fixture itself.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let storage = StorageManager::with_base_dir(base);
        (tmp, storage)
    }

    #[test]
    fn empty_install_reports_none_and_no_findings() {
        let (_tmp, storage) = setup();
        let report = collect_status(&storage, Browser::Chrome).unwrap();
        assert_eq!(report.active_profile, None);
        assert!(report.profiles.is_empty());
        assert!(report.doctor.issues.is_empty());
        let text = render_status(&report);
        assert!(text.contains("Active profile:") && text.contains("none"));
        assert!(text.contains("No profiles yet"));
    }

    #[test]
    fn profiles_report_running_proxy_and_backup() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage
            .create_profile("work", Some("9-5"), browser)
            .unwrap();
        storage.create_profile("play", None, browser).unwrap();
        storage
            .set_proxy(
                "work",
                browser,
                Some(crate::proxy::ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap()),
            )
            .unwrap();

        // A live process (this test) holds "work"; a stale lock on "play".
        std::os::unix::fs::symlink(
            format!("host-{}", std::process::id()),
            storage.profile_dir(browser, "work").join("SingletonLock"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "host-999999999",
            storage.profile_dir(browser, "play").join("SingletonLock"),
        )
        .unwrap();

        // One backup for "work".
        let backups = storage.base_dir().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("chrome-work-20261027120000.tar.zst"), "x").unwrap();
        std::fs::write(backups.join("chrome-work-20261027130000.tar.zst.age"), "x").unwrap();

        let report = collect_status(&storage, browser).unwrap();
        let work = report.profiles.iter().find(|p| p.name == "work").unwrap();
        let play = report.profiles.iter().find(|p| p.name == "play").unwrap();

        assert!(work.running);
        assert_eq!(work.running_pid, Some(std::process::id()));
        assert_eq!(work.proxy.as_deref(), Some("socks5://10.0.0.1:1080"));
        let backup = work.latest_backup.as_ref().unwrap();
        assert!(backup.encrypted, "newest archive (.age) wins");
        assert_eq!(
            backup.created_at.format("%Y%m%d%H%M%S").to_string(),
            "20261027130000"
        );
        assert!(!play.running, "stale lock must not read as running");
        assert!(play.latest_backup.is_none());

        // The human render carries the essentials on one screen.
        let text = render_status(&report);
        assert!(text.contains("work"));
        assert!(text.contains(&format!("run {}", std::process::id())));
        assert!(text.contains("socks5://10.0.0.1:1080"));
        assert!(text.contains("age"));
    }

    #[test]
    fn doctor_findings_surface_in_the_report() {
        let (tmp, storage) = setup();
        let browser = Browser::Chrome;
        // Dangling default-dir symlink: fixable doctor finding.
        let default_link = tmp.path().join("default-link");
        let storage = storage.with_default_dir_override(default_link.clone());
        std::os::unix::fs::symlink(tmp.path().join("missing"), &default_link).unwrap();

        let report = collect_status(&storage, browser).unwrap();
        assert_eq!(report.doctor.issues.len(), 1);
        assert_eq!(report.doctor.issues[0].check, "default-symlink");
        assert_eq!(report.doctor.auto_fixable, 1);
        let text = render_status(&report);
        assert!(text.contains("default-symlink"));
        assert!(text.contains("fixable"));
    }
}
