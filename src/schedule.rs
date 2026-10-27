//! Scheduled auto-backup: a daily `bm backup --all` installed as a launchd
//! LaunchAgent (macOS) or a systemd user timer (Linux).
//!
//! One unit per browser, keyed by the selected `--browser`: label
//! `io.github.limmytian.bm-backup.<browser>`, running the resolved bm binary
//! with `backup --all [--keep N] --browser <browser>`. Installing over an
//! existing schedule is refused — uninstall first. The generated unit files
//! are plain strings assembled here and unit-tested; the platform helpers
//! only load/unload them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::StorageManager;

/// Label / unit-name stem, one unit per browser.
fn unit_name(browser: Browser) -> String {
    format!("io.github.limmytian.bm-backup.{browser}")
}

/// Retention applied when the user does not pass --keep at install time:
/// an unbounded daily snapshot would eventually fill the disk.
pub const DEFAULT_KEEP: usize = 7;

/// Where scheduled-run output lands (launchd StandardOut/ErrorPath).
fn log_path(storage: &StorageManager) -> PathBuf {
    storage.base_dir().join("backup.log")
}

fn scheduled_argv(exe: &str, browser: Browser, keep: usize) -> Vec<String> {
    vec![
        exe.to_string(),
        "backup".into(),
        "--all".into(),
        "--keep".into(),
        keep.to_string(),
        "--browser".into(),
        browser.to_string(),
    ]
}

/// launchd LaunchAgent plist for the daily backup run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist(storage: &StorageManager, browser: Browser, keep: usize) -> Result<String> {
    let exe = current_exe()?;
    let argv = scheduled_argv(&exe, browser, keep);
    let args_xml: String = argv
        .iter()
        .map(|a| format!("        <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");
    let log = xml_escape(&log_path(storage).display().to_string());
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key>
        <integer>3</integer>
        <key>Minute</key>
        <integer>0</integer>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = unit_name(browser),
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// systemd user service + timer pair for the daily backup run.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_service(storage: &StorageManager, browser: Browser, keep: usize) -> Result<String> {
    let exe = current_exe()?;
    let argv = scheduled_argv(&exe, browser, keep);
    Ok(format!(
        r#"[Unit]
Description=bm daily profile backup ({browser})
After=network-online.target

[Service]
Type=oneshot
ExecStart={args}
StandardOutput=append:{log}
StandardError=append:{log}
"#,
        args = shell_join(&argv),
        log = log_path(storage).display(),
    ))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_timer(browser: Browser) -> String {
    format!(
        r#"[Unit]
Description=Daily bm profile backup timer ({browser})

[Timer]
OnCalendar=*-*-* 03:00:00
Persistent=true
Unit={service}.service

[Install]
WantedBy=timers.target
"#,
        service = unit_name(browser),
    )
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Absolute, symlink-resolved path of the running bm binary — the scheduled
/// unit must not depend on PATH.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn current_exe() -> Result<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .ok_or_else(|| {
            BmError::Other("cannot resolve the bm binary path for the scheduled unit".into())
        })
}

// --- platform loaders -------------------------------------------------------

#[cfg(target_os = "macos")]
fn load_unit(plist: &Path) -> Result<()> {
    // Replace any previously loaded instance, then load the new file. The
    // pre-unload is best-effort (usually nothing is loaded) — keep its
    // launchctl chatter off the user's terminal.
    let _ = Command::new("launchctl")
        .args(["unload", &plist.display().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let status = Command::new("launchctl")
        .args(["load", &plist.display().to_string()])
        .status()
        .map_err(|e| BmError::Other(format!("cannot run launchctl: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(BmError::Other(format!(
            "launchctl load failed — install the agent manually: launchctl load {}",
            plist.display()
        )))
    }
}

#[cfg(target_os = "macos")]
fn unload_unit(plist: &Path) -> Result<()> {
    Command::new("launchctl")
        .args(["unload", &plist.display().to_string()])
        .status()
        .map_err(|e| BmError::Other(format!("cannot run launchctl: {e}")))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn unit_loaded(label: &str) -> bool {
    Command::new("launchctl")
        .args(["list", label])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .map_err(|e| {
            BmError::Other(format!(
                "cannot run systemctl (is systemd user session available?): {e}"
            ))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(BmError::Other(format!(
            "systemctl --user {} failed",
            args.join(" ")
        )))
    }
}

// --- user-facing schedule actions -------------------------------------------

/// Directory holding user LaunchAgents (macOS convention).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/LaunchAgents")
}

/// Directory holding user systemd units (Linux convention).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_unit_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/systemd/user")
}

/// `bm backup --schedule install`: write the unit files for the selected
/// browser and load them. Refuses to replace an existing schedule.
pub fn install(storage: &StorageManager, browser: Browser, keep: Option<usize>) -> Result<String> {
    let keep = keep.unwrap_or(DEFAULT_KEEP);

    #[cfg(target_os = "macos")]
    {
        install_macos(storage, browser, keep)
    }

    #[cfg(target_os = "linux")]
    {
        install_linux(storage, browser, keep)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (storage, browser, keep);
        Err(BmError::UnsupportedPlatform(
            "scheduled backups require macOS (launchd) or Linux (systemd user timers)".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn install_macos(storage: &StorageManager, browser: Browser, keep: usize) -> Result<String> {
    {
        let dir = launchd_plist_dir();
        let plist = dir.join(format!("{}.plist", unit_name(browser)));
        if plist.exists() {
            return Err(BmError::Other(format!(
                "a backup schedule already exists: {} — uninstall it first",
                plist.display()
            )));
        }
        fs::create_dir_all(&dir)?;
        fs::write(&plist, launchd_plist(storage, browser, keep)?)?;
        load_unit(&plist)?;
        Ok(format!(
            "Installed daily 03:00 backup (launchd agent {}).\n  Unit: {}\n  Runs: {} backup --all --keep {keep} --browser {browser}\n  Logs: {}",
            unit_name(browser),
            plist.display(),
            current_exe().unwrap_or_else(|_| "bm".into()),
            log_path(storage).display(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn install_linux(storage: &StorageManager, browser: Browser, keep: usize) -> Result<String> {
    {
        let dir = systemd_unit_dir();
        let service = dir.join(format!("{}.service", unit_name(browser)));
        let timer = dir.join(format!("{}.timer", unit_name(browser)));
        if service.exists() || timer.exists() {
            return Err(BmError::Other(format!(
                "a backup schedule already exists ({}) — uninstall it first",
                timer.display()
            )));
        }
        fs::create_dir_all(&dir)?;
        fs::write(&service, systemd_service(storage, browser, keep)?)?;
        fs::write(&timer, systemd_timer(browser))?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", &format!("{}.timer", unit_name(browser))])?;
        Ok(format!(
            "Installed daily 03:00 backup (systemd user timer).\n  Unit: {}\n  Runs: {} backup --all --keep {keep} --browser {browser}\n  Logs: journalctl --user -u {}",
            timer.display(),
            current_exe().unwrap_or_else(|_| "bm".into()),
            unit_name(browser),
        ))
    }
}

/// `bm backup --schedule uninstall`.
pub fn uninstall(_storage: &StorageManager, browser: Browser) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        uninstall_macos(browser)
    }

    #[cfg(target_os = "linux")]
    {
        uninstall_linux(browser)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = browser;
        Err(BmError::UnsupportedPlatform(
            "scheduled backups require macOS (launchd) or Linux (systemd user timers)".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn uninstall_macos(browser: Browser) -> Result<String> {
    {
        let plist = launchd_plist_dir().join(format!("{}.plist", unit_name(browser)));
        if !plist.exists() {
            return Err(BmError::Other(format!(
                "no backup schedule installed (expected {})",
                plist.display()
            )));
        }
        unload_unit(&plist)?;
        fs::remove_file(&plist)?;
        Ok(format!("Removed scheduled backup ({}).", plist.display()))
    }
}

#[cfg(target_os = "linux")]
fn uninstall_linux(browser: Browser) -> Result<String> {
    {
        let dir = systemd_unit_dir();
        let timer = dir.join(format!("{}.timer", unit_name(browser)));
        let service = dir.join(format!("{}.service", unit_name(browser)));
        if !timer.exists() && !service.exists() {
            return Err(BmError::Other(format!(
                "no backup schedule installed (expected {})",
                timer.display()
            )));
        }
        let _ = systemctl(&["disable", "--now", &format!("{}.timer", unit_name(browser))]);
        if timer.exists() {
            fs::remove_file(&timer)?;
        }
        if service.exists() {
            fs::remove_file(&service)?;
        }
        systemctl(&["daemon-reload"])?;
        Ok(format!(
            "Removed scheduled backup ({}).",
            unit_name(browser)
        ))
    }
}

/// `bm backup --schedule status`.
pub fn status(storage: &StorageManager, browser: Browser) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        status_macos(storage, browser)
    }

    #[cfg(target_os = "linux")]
    {
        status_linux(browser)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (storage, browser);
        Err(BmError::UnsupportedPlatform(
            "scheduled backups require macOS (launchd) or Linux (systemd user timers)".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn status_macos(storage: &StorageManager, browser: Browser) -> Result<String> {
    {
        let plist = launchd_plist_dir().join(format!("{}.plist", unit_name(browser)));
        let loaded = unit_loaded(&unit_name(browser));
        Ok(match (plist.exists(), loaded) {
            (true, true) => format!(
                "installed and loaded: {} (daily 03:00, logs: {})",
                plist.display(),
                log_path(storage).display()
            ),
            (true, false) => format!(
                "installed but not loaded: {} — load with `launchctl load {}`",
                plist.display(),
                plist.display()
            ),
            (false, _) => format!("not installed (expected {})", plist.display()),
        })
    }
}

#[cfg(target_os = "linux")]
fn status_linux(browser: Browser) -> Result<String> {
    {
        let timer = systemd_unit_dir().join(format!("{}.timer", unit_name(browser)));
        if !timer.exists() {
            return Ok(format!("not installed (expected {})", timer.display()));
        }
        let enabled = Command::new("systemctl")
            .arg("--user")
            .args(["is-enabled", &format!("{}.timer", unit_name(browser))])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        Ok(format!(
            "installed ({}), timer {enabled}; logs: journalctl --user -u {}",
            timer.display(),
            unit_name(browser)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        (tmp, StorageManager::with_base_dir(base))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_carries_the_daily_backup_run() {
        let (_tmp, storage) = setup();
        let plist = launchd_plist(&storage, Browser::Chrome, DEFAULT_KEEP).unwrap();
        assert!(plist.contains(&format!("<string>{}</string>", unit_name(Browser::Chrome))));
        assert!(plist.contains("<key>StartCalendarInterval</key>"));
        assert!(plist.contains("<integer>3</integer>"));
        assert!(plist.contains("--all"));
        assert!(plist.contains("--keep"));
        assert!(plist.contains(&DEFAULT_KEEP.to_string()));
        assert!(plist.contains("<string>--browser</string>"));
        assert!(plist.contains("<string>chrome</string>"));
        // The binary path must be absolute: launchd has no useful PATH.
        let exe_line = plist.lines().find(|l| l.contains("/bm")).unwrap();
        assert!(
            exe_line.trim_start().starts_with('<'),
            "exe embedded as string element"
        );
        assert!(exe_line.contains('/'));
        // Log output is captured, never dropped.
        assert!(plist.contains(&log_path(&storage).display().to_string()));
    }

    #[test]
    fn systemd_files_carries_the_daily_backup_run() {
        let (_tmp, storage) = setup();
        let service = systemd_service(&storage, Browser::Edge, 5).unwrap();
        assert!(service.contains("[Service]"));
        assert!(service.contains("--all --keep 5 --browser edge"));
        assert!(service.contains("Type=oneshot"));

        let timer = systemd_timer(Browser::Edge);
        assert!(timer.contains("OnCalendar=*-*-* 03:00:00"));
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains(&format!("Unit={}.service", unit_name(Browser::Edge))));
    }

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(shell_join(&["bm".into(), "backup".into()]), "bm backup");
        assert_eq!(shell_join(&["my bm".into()]), "'my bm'");
        assert_eq!(shell_join(&["it's".into()]), "'it'\\''s'");
    }

    #[test]
    fn unit_names_are_per_browser() {
        assert_eq!(
            unit_name(Browser::Chrome),
            "io.github.limmytian.bm-backup.chrome"
        );
        assert_ne!(unit_name(Browser::Chrome), unit_name(Browser::Brave));
    }
}
