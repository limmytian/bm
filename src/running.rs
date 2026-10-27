//! Per-profile running detection.
//!
//! Chromium keeps a `SingletonLock` symlink inside every *in-use*
//! user-data-dir (macOS and Linux alike, see `process_singleton_posix.cc`);
//! its target is `<hostname>-<pid>`. A profile "is running" when that
//! symlink exists and the recorded pid is alive. A stale lock (crash) fails
//! the pid liveness check, so it does not block operations.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::Result;

/// The pid recorded in the profile's `SingletonLock`, when the symlink
/// exists and points at a parseable `<hostname>-<pid>` target.
pub fn singleton_pid(profile_dir: &Path) -> Option<u32> {
    let target = fs::read_link(profile_dir.join("SingletonLock")).ok()?;
    let raw = target.to_string_lossy();
    // Target is "<hostname>-<pid>"; hostnames may contain dashes, so split
    // at the LAST dash.
    raw.rsplit('-').next()?.parse().ok()
}

/// Whether `pid` refers to a live process we can signal (`kill -0`).
pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The live pid of the browser holding `profile_dir`, if any — so callers
/// can report *which* process blocks them.
pub fn running_pid(profile_dir: &Path) -> Result<Option<u32>> {
    Ok(singleton_pid(profile_dir).filter(|pid| pid_alive(*pid)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_singleton_lock_means_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(singleton_pid(tmp.path()), None);
        assert_eq!(running_pid(tmp.path()).unwrap(), None);
    }

    #[test]
    fn singleton_lock_with_live_pid_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        // This test process is alive by construction.
        std::os::unix::fs::symlink(
            format!("some-host-{}", std::process::id()),
            tmp.path().join("SingletonLock"),
        )
        .unwrap();

        assert_eq!(singleton_pid(tmp.path()), Some(std::process::id()));
        assert_eq!(running_pid(tmp.path()).unwrap(), Some(std::process::id()));
    }

    #[test]
    fn stale_lock_with_dead_pid_is_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        // Any implausibly large pid fails the liveness probe (real pid
        // spaces top out far below this on macOS and Linux).
        std::os::unix::fs::symlink("host-999999999", tmp.path().join("SingletonLock")).unwrap();

        assert_eq!(singleton_pid(tmp.path()), Some(999999999));
        assert_eq!(running_pid(tmp.path()).unwrap(), None);
    }

    #[test]
    fn malformed_lock_target_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        // Not a symlink: read_link fails.
        fs::write(tmp.path().join("SingletonLock"), "regular file").unwrap();
        assert_eq!(singleton_pid(tmp.path()), None);

        // Symlink with an unparseable pid.
        let lock = tmp.path().join("SingletonLock");
        fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink("hostname-without-number", &lock).unwrap();
        assert_eq!(singleton_pid(tmp.path()), None);
    }
}
