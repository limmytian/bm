//! Agent lease lock protocol (`bm lease claim/release/status`).
//!
//! Provides mutual exclusion against destructive operations (switching,
//! cleaning, renaming, deleting) while an autonomous Agent or batch process
//! holds an active lease on a browser profile.
//!
//! Stored in `~/.bm/leases.json` with 0600 file permissions.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::StorageManager;

pub const DEFAULT_LEASE_TTL_SECS: u64 = 300; // 5 minutes

/// An active lease lock held by an agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lease {
    pub browser: Browser,
    pub profile: String,
    pub holder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub claimed_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Lease {
    /// Check whether this lease is currently valid and not expired or abandoned.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if now >= self.expires_at {
            return false;
        }
        // If a PID was recorded, verify process liveness
        if let Some(pid) = self.pid {
            if !crate::running::pid_alive(pid) {
                return false;
            }
        }
        true
    }

    /// Remaining duration in seconds.
    pub fn remaining_seconds(&self, now: DateTime<Utc>) -> i64 {
        (self.expires_at - now).num_seconds().max(0)
    }
}

/// Persistent registry of active leases.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LeaseRegistry {
    #[serde(default)]
    pub leases: HashMap<String, Lease>,
}

fn lease_key(browser: Browser, profile: &str) -> String {
    format!("{browser}:{profile}")
}

/// Load the lease registry from `~/.bm/leases.json`.
pub fn load_leases(storage: &StorageManager) -> Result<LeaseRegistry> {
    let file = storage.leases_file();
    if !file.exists() {
        return Ok(LeaseRegistry::default());
    }

    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) => return Err(BmError::Io(e)),
    };

    let reg: LeaseRegistry = serde_json::from_str(&content).unwrap_or_default();
    Ok(reg)
}

/// Atomically save the lease registry to `~/.bm/leases.json` with 0600 permissions.
pub fn save_leases(storage: &StorageManager, registry: &LeaseRegistry) -> Result<()> {
    let target = storage.leases_file();
    let tmp = target.with_extension("tmp");

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_vec_pretty(registry)?;

    let mut file = File::create(&tmp)?;
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o600);
        let _ = file.set_permissions(perms);
    }
    file.write_all(&payload)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp, &target)?;
    Ok(())
}

/// Options for claiming an agent lease.
#[derive(Debug, Clone, Default)]
pub struct ClaimOptions<'a> {
    pub holder: &'a str,
    pub ttl_secs: Option<u64>,
    pub reason: Option<&'a str>,
    pub pid: Option<u32>,
    pub force: bool,
}

/// Claim or refresh a lease on a profile.
pub fn claim_lease(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    opts: ClaimOptions<'_>,
) -> Result<Lease> {
    StorageManager::validate_name(profile)?;

    // Profile must exist
    let profile_dir = storage.profile_dir(browser, profile);
    if !profile_dir.exists() {
        return Err(BmError::ProfileNotFound {
            name: profile.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    let mut reg = load_leases(storage)?;
    let key = lease_key(browser, profile);
    let now = Utc::now();

    // Check if an existing valid lease is held by someone else
    if let Some(existing) = reg.leases.get(&key) {
        if existing.is_active(now) && !opts.force && existing.holder != opts.holder {
            return Err(BmError::ProfileLeased {
                profile: profile.to_string(),
                browser: browser.display_name().to_string(),
                holder: existing.holder.clone(),
                remaining_secs: existing.remaining_seconds(now),
            });
        }
    }

    let ttl = opts.ttl_secs.unwrap_or(DEFAULT_LEASE_TTL_SECS);
    let expires_at = now + Duration::seconds(ttl as i64);

    let lease = Lease {
        browser,
        profile: profile.to_string(),
        holder: opts.holder.to_string(),
        pid: opts.pid,
        reason: opts.reason.map(str::to_string),
        claimed_at: now,
        heartbeat_at: now,
        expires_at,
    };

    reg.leases.insert(key, lease.clone());
    save_leases(storage, &reg)?;
    Ok(lease)
}

/// Release a lease on a profile.
pub fn release_lease(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    holder: Option<&str>,
    force: bool,
) -> Result<()> {
    StorageManager::validate_name(profile)?;

    let mut reg = load_leases(storage)?;
    let key = lease_key(browser, profile);

    match reg.leases.get(&key) {
        Some(existing) => {
            if !force {
                if let Some(h) = holder {
                    if existing.holder != h {
                        return Err(BmError::Other(format!(
                            "Cannot release lease held by '{}' (requested by '{}'); use --force to break.",
                            existing.holder, h
                        )));
                    }
                }
            }
            reg.leases.remove(&key);
            save_leases(storage, &reg)?;
            Ok(())
        }
        None => Err(BmError::LeaseNotFound {
            profile: profile.to_string(),
            browser: browser.display_name().to_string(),
        }),
    }
}

/// Extend an existing lease's expiration.
pub fn heartbeat_lease(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    ttl_secs: Option<u64>,
) -> Result<Lease> {
    StorageManager::validate_name(profile)?;

    let mut reg = load_leases(storage)?;
    let key = lease_key(browser, profile);
    let now = Utc::now();

    match reg.leases.get_mut(&key) {
        Some(lease) => {
            let ttl = ttl_secs.unwrap_or(DEFAULT_LEASE_TTL_SECS);
            lease.heartbeat_at = now;
            lease.expires_at = now + Duration::seconds(ttl as i64);
            let updated = lease.clone();
            save_leases(storage, &reg)?;
            Ok(updated)
        }
        None => Err(BmError::LeaseNotFound {
            profile: profile.to_string(),
            browser: browser.display_name().to_string(),
        }),
    }
}

/// Check if a profile is currently locked by a lease.
pub fn check_lease(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
) -> Result<Option<Lease>> {
    let reg = load_leases(storage)?;
    let key = lease_key(browser, profile);
    let now = Utc::now();

    if let Some(lease) = reg.leases.get(&key) {
        if lease.is_active(now) {
            return Ok(Some(lease.clone()));
        }
    }
    Ok(None)
}

/// Guard a destructive operation: returns an error if the profile has an active lease and `!force`.
pub fn guard_lease(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    force: bool,
) -> Result<()> {
    if force {
        return Ok(());
    }
    if let Some(lease) = check_lease(storage, browser, profile)? {
        let now = Utc::now();
        let remaining_secs = lease.remaining_seconds(now);
        return Err(BmError::ProfileLeased {
            profile: profile.to_string(),
            browser: browser.display_name().to_string(),
            holder: lease.holder,
            remaining_secs,
        });
    }
    Ok(())
}

/// Inspect all active leases, optionally filtered by browser and profile.
pub fn list_leases(
    storage: &StorageManager,
    filter_browser: Option<Browser>,
    filter_profile: Option<&str>,
) -> Result<Vec<Lease>> {
    let reg = load_leases(storage)?;

    let mut list: Vec<Lease> = reg
        .leases
        .into_values()
        .filter(|l| {
            if let Some(b) = filter_browser {
                if l.browser != b {
                    return false;
                }
            }
            if let Some(p) = filter_profile {
                if l.profile != p {
                    return false;
                }
            }
            true
        })
        .collect();

    list.sort_by(|a, b| {
        (&a.browser.to_string(), &a.profile).cmp(&(&b.browser.to_string(), &b.profile))
    });
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
        (tmp, storage)
    }

    #[test]
    fn lease_lifecycle_and_guarding() {
        let (_tmp, storage) = test_storage();
        let browser = Browser::Chrome;

        // Create profile
        storage
            .create_profile("target-prof", None, browser)
            .unwrap();

        // 1. Initial check: not locked
        assert!(check_lease(&storage, browser, "target-prof")
            .unwrap()
            .is_none());
        assert!(guard_lease(&storage, browser, "target-prof", false).is_ok());

        // 2. Claim lease
        let lease = claim_lease(
            &storage,
            browser,
            "target-prof",
            ClaimOptions {
                holder: "agent-worker-1",
                ttl_secs: Some(60),
                reason: Some("Running CDP scraper"),
                pid: Some(std::process::id()),
                force: false,
            },
        )
        .unwrap();

        assert_eq!(lease.holder, "agent-worker-1");
        assert!(lease.is_active(Utc::now()));

        // Guard now rejects non-force actions
        let err = guard_lease(&storage, browser, "target-prof", false).unwrap_err();
        assert!(matches!(err, BmError::ProfileLeased { .. }));

        // Force passes
        assert!(guard_lease(&storage, browser, "target-prof", true).is_ok());

        // Competing claim without force is rejected
        let compete_err = claim_lease(
            &storage,
            browser,
            "target-prof",
            ClaimOptions {
                holder: "agent-worker-2",
                ttl_secs: Some(60),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(compete_err, BmError::ProfileLeased { .. }));

        // Same holder can refresh lease
        let refreshed = claim_lease(
            &storage,
            browser,
            "target-prof",
            ClaimOptions {
                holder: "agent-worker-1",
                ttl_secs: Some(120),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(refreshed.holder, "agent-worker-1");

        // Heartbeat extends expiration
        let hb = heartbeat_lease(&storage, browser, "target-prof", Some(300)).unwrap();
        assert!(hb.remaining_seconds(Utc::now()) >= 290);

        // Release lease
        release_lease(
            &storage,
            browser,
            "target-prof",
            Some("agent-worker-1"),
            false,
        )
        .unwrap();
        assert!(check_lease(&storage, browser, "target-prof")
            .unwrap()
            .is_none());
        assert!(guard_lease(&storage, browser, "target-prof", false).is_ok());
    }

    #[test]
    fn expired_or_dead_pid_leases_are_inactive() {
        let lease_expired = Lease {
            browser: Browser::Chrome,
            profile: "test".into(),
            holder: "old-agent".into(),
            pid: None,
            reason: None,
            claimed_at: Utc::now() - Duration::seconds(600),
            heartbeat_at: Utc::now() - Duration::seconds(600),
            expires_at: Utc::now() - Duration::seconds(100),
        };
        assert!(!lease_expired.is_active(Utc::now()));

        let lease_dead_pid = Lease {
            browser: Browser::Chrome,
            profile: "test".into(),
            holder: "crashed-agent".into(),
            pid: Some(999999999), // Implausibly high pid
            reason: None,
            claimed_at: Utc::now(),
            heartbeat_at: Utc::now(),
            expires_at: Utc::now() + Duration::seconds(600),
        };
        assert!(!lease_dead_pid.is_active(Utc::now()));
    }
}
