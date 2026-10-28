//! Multi-profile workspaces and session groups (`bm group`).
//!
//! Models and persists named groups in `~/.bm/groups.json`:
//! - Ordered member list: (browser, profile name, optional proxy/url overrides)
//! - Atomic load/save with permission checks (0600)
//! - Concurrent group launch (`bm group open`)
//! - Graceful multi-process group teardown (`bm group close`)
//! - Group status and health reporting (`bm group list/show/status`)

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;
use crate::storage::StorageManager;

pub const GROUPS_SCHEMA_VERSION: u32 = 1;

fn current_groups_schema_version() -> u32 {
    GROUPS_SCHEMA_VERSION
}

/// A member profile of a session group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMember {
    pub browser: Browser,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

/// A named session group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<GroupMember>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Shape of `~/.bm/groups.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupsRegistry {
    #[serde(default = "current_groups_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub groups: HashMap<String, Group>,
}

impl Default for GroupsRegistry {
    fn default() -> Self {
        Self {
            schema_version: GROUPS_SCHEMA_VERSION,
            groups: HashMap::new(),
        }
    }
}

/// Launch outcome for an individual group member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberLaunchOutcome {
    pub browser: Browser,
    pub profile: String,
    pub pid: Option<u32>,
    pub error: Option<String>,
    pub cdp_endpoint: Option<String>,
}

/// Report for `bm group open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupLaunchReport {
    pub group: String,
    pub members: Vec<GroupMemberLaunchOutcome>,
}

/// Close outcome for an individual group member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberCloseOutcome {
    pub browser: Browser,
    pub profile: String,
    pub pid: Option<u32>,
    pub method: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Report for `bm group close`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupCloseReport {
    pub group: String,
    pub members: Vec<GroupMemberCloseOutcome>,
}

/// Live status of a group member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberStatus {
    pub browser: Browser,
    pub profile: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub proxy: Option<String>,
    pub cdp_endpoint: Option<String>,
    pub urls: Vec<String>,
}

/// Report for `bm group status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupStatusReport {
    pub name: String,
    pub description: Option<String>,
    pub member_count: usize,
    pub running_count: usize,
    pub members: Vec<GroupMemberStatus>,
}

/// Load the groups registry from `~/.bm/groups.json`. Returns an empty
/// registry if the file does not exist.
pub fn load_groups(storage: &StorageManager) -> Result<GroupsRegistry> {
    let file = storage.groups_file();
    if !file.exists() {
        return Ok(GroupsRegistry::default());
    }

    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) => return Err(BmError::Io(e)),
    };

    let reg: GroupsRegistry = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => {
            // Corrupt file falls back to empty default
            return Ok(GroupsRegistry::default());
        }
    };

    Ok(reg)
}

/// Save the groups registry atomically to `~/.bm/groups.json` using a temporary file.
pub fn save_groups(storage: &StorageManager, registry: &GroupsRegistry) -> Result<()> {
    let target = storage.groups_file();
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

/// Create a new empty group.
pub fn create_group(
    storage: &StorageManager,
    name: &str,
    description: Option<&str>,
) -> Result<Group> {
    StorageManager::validate_name(name)?;

    let mut reg = load_groups(storage)?;
    if reg.groups.contains_key(name) {
        return Err(BmError::GroupAlreadyExists {
            name: name.to_string(),
        });
    }

    let now = Utc::now();
    let group = Group {
        name: name.to_string(),
        description: description.map(str::to_string),
        members: Vec::new(),
        created_at: now,
        updated_at: now,
    };

    reg.groups.insert(name.to_string(), group.clone());
    save_groups(storage, &reg)?;
    Ok(group)
}

/// Delete a group by name.
pub fn delete_group(storage: &StorageManager, name: &str) -> Result<()> {
    StorageManager::validate_name(name)?;

    let mut reg = load_groups(storage)?;
    if reg.groups.remove(name).is_none() {
        return Err(BmError::GroupNotFound {
            name: name.to_string(),
        });
    }

    save_groups(storage, &reg)?;
    Ok(())
}

/// Get a group by name.
pub fn get_group(storage: &StorageManager, name: &str) -> Result<Group> {
    StorageManager::validate_name(name)?;
    let reg = load_groups(storage)?;
    reg.groups
        .get(name)
        .cloned()
        .ok_or_else(|| BmError::GroupNotFound {
            name: name.to_string(),
        })
}

/// List all groups sorted by name.
pub fn list_groups(storage: &StorageManager) -> Result<Vec<Group>> {
    let reg = load_groups(storage)?;
    let mut groups: Vec<Group> = reg.groups.into_values().collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(groups)
}

/// Add or update a member in a group.
pub fn add_member(
    storage: &StorageManager,
    group_name: &str,
    browser: Browser,
    profile: &str,
    urls: Vec<String>,
    proxy: Option<ProxyConfig>,
) -> Result<Group> {
    StorageManager::validate_name(group_name)?;
    StorageManager::validate_name(profile)?;

    // Verify profile exists in storage
    let profile_dir = storage.profile_dir(browser, profile);
    if !profile_dir.exists() {
        return Err(BmError::ProfileNotFound {
            name: profile.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    let mut reg = load_groups(storage)?;
    let now = Utc::now();

    let group = match reg.groups.get_mut(group_name) {
        Some(g) => g,
        None => {
            let new_group = Group {
                name: group_name.to_string(),
                description: None,
                members: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            reg.groups.insert(group_name.to_string(), new_group);
            reg.groups.get_mut(group_name).unwrap()
        }
    };

    if let Some(existing) = group
        .members
        .iter_mut()
        .find(|m| m.browser == browser && m.profile == profile)
    {
        existing.urls = urls;
        existing.proxy = proxy;
    } else {
        group.members.push(GroupMember {
            browser,
            profile: profile.to_string(),
            urls,
            proxy,
        });
    }

    group.updated_at = now;
    let saved = group.clone();
    save_groups(storage, &reg)?;
    Ok(saved)
}

/// Remove a member from a group.
pub fn remove_member(
    storage: &StorageManager,
    group_name: &str,
    browser: Browser,
    profile: &str,
) -> Result<Group> {
    StorageManager::validate_name(group_name)?;
    StorageManager::validate_name(profile)?;

    let mut reg = load_groups(storage)?;
    let group = reg
        .groups
        .get_mut(group_name)
        .ok_or_else(|| BmError::GroupNotFound {
            name: group_name.to_string(),
        })?;

    let initial_len = group.members.len();
    group
        .members
        .retain(|m| !(m.browser == browser && m.profile == profile));

    if group.members.len() == initial_len {
        return Err(BmError::GroupMemberNotFound {
            group: group_name.to_string(),
            profile: profile.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    group.updated_at = Utc::now();
    let saved = group.clone();
    save_groups(storage, &reg)?;
    Ok(saved)
}

/// Launch all members in a group concurrently side by side.
pub fn open_group(
    storage: &StorageManager,
    group_name: &str,
    extra_urls: &[String],
    cdp: bool,
) -> Result<GroupLaunchReport> {
    let group = get_group(storage, group_name)?;
    let mut outcomes = Vec::new();

    for member in &group.members {
        let binary_res = member.browser.binary_path();
        let binary = match binary_res {
            Ok(b) => b,
            Err(e) => {
                outcomes.push(GroupMemberLaunchOutcome {
                    browser: member.browser,
                    profile: member.profile.clone(),
                    pid: None,
                    error: Some(e.to_string()),
                    cdp_endpoint: None,
                });
                continue;
            }
        };

        let profile_dir = storage.profile_dir(member.browser, &member.profile);
        if !profile_dir.exists() {
            outcomes.push(GroupMemberLaunchOutcome {
                browser: member.browser,
                profile: member.profile.clone(),
                pid: None,
                error: Some(format!(
                    "Profile '{}' not found for {}",
                    member.profile,
                    member.browser.display_name()
                )),
                cdp_endpoint: None,
            });
            continue;
        }

        let proxy_owned = storage.proxy_for(member.browser, &member.profile);
        let proxy = member.proxy.as_ref().or(proxy_owned.as_ref());

        let mut member_urls = member.urls.clone();
        member_urls.extend_from_slice(extra_urls);

        let mut args = crate::open::launch_args(&profile_dir, proxy, &member_urls);
        if cdp {
            args.extend(crate::cdp::cdp_launch_flags());
        }

        let spawned = Command::new(&binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match spawned {
            Ok(child) => {
                let pid = child.id();
                let mut cdp_endpoint = None;

                if cdp {
                    if let Ok(mut entry) = crate::cdp::wait_for_endpoint(&profile_dir) {
                        entry.pid = Some(pid);
                        let ws = entry.ws_url();
                        let mut reg = crate::cdp::load_registry(storage);
                        reg.insert(format!("{}:{}", member.browser, member.profile), entry);
                        let _ = crate::cdp::save_registry(storage, &reg);
                        cdp_endpoint = Some(ws);
                    }
                }

                outcomes.push(GroupMemberLaunchOutcome {
                    browser: member.browser,
                    profile: member.profile.clone(),
                    pid: Some(pid),
                    error: None,
                    cdp_endpoint,
                });
            }
            Err(e) => {
                outcomes.push(GroupMemberLaunchOutcome {
                    browser: member.browser,
                    profile: member.profile.clone(),
                    pid: None,
                    error: Some(e.to_string()),
                    cdp_endpoint: None,
                });
            }
        }
    }

    Ok(GroupLaunchReport {
        group: group_name.to_string(),
        members: outcomes,
    })
}

/// Gracefully shut down all running browser instances in a group.
pub fn close_group(
    storage: &StorageManager,
    group_name: &str,
    force: bool,
) -> Result<GroupCloseReport> {
    let group = get_group(storage, group_name)?;
    let mut outcomes = Vec::new();

    for member in &group.members {
        let profile_dir = storage.profile_dir(member.browser, &member.profile);
        let pid_opt = crate::running::running_pid(&profile_dir)?;

        let pid = match pid_opt {
            Some(p) => p,
            None => {
                outcomes.push(GroupMemberCloseOutcome {
                    browser: member.browser,
                    profile: member.profile.clone(),
                    pid: None,
                    method: "not_running".to_string(),
                    success: true,
                    error: None,
                });
                continue;
            }
        };

        // 1. Attempt graceful CDP Browser.close if recorded
        let cdp_key = format!("{}:{}", member.browser, member.profile);
        let mut cdp_reg = crate::cdp::load_registry(storage);
        let mut closed_via_cdp = false;

        if let Some(entry) = cdp_reg.remove(&cdp_key) {
            if crate::cdp::port_alive(entry.port)
                && crate::cdp::close_via_browser_close(&entry).is_ok()
            {
                let _ = crate::cdp::save_registry(storage, &cdp_reg);
                // Wait up to 1 second for termination
                for _ in 0..10 {
                    if !crate::running::pid_alive(pid) {
                        closed_via_cdp = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }

        if closed_via_cdp {
            outcomes.push(GroupMemberCloseOutcome {
                browser: member.browser,
                profile: member.profile.clone(),
                pid: Some(pid),
                method: "cdp".to_string(),
                success: true,
                error: None,
            });
            continue;
        }

        // 2. SIGTERM
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output();

        let mut exited = false;
        for _ in 0..10 {
            if !crate::running::pid_alive(pid) {
                exited = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        if exited {
            outcomes.push(GroupMemberCloseOutcome {
                browser: member.browser,
                profile: member.profile.clone(),
                pid: Some(pid),
                method: "sigterm".to_string(),
                success: true,
                error: None,
            });
            continue;
        }

        // 3. SIGKILL if force
        if force {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
            std::thread::sleep(std::time::Duration::from_millis(200));

            if !crate::running::pid_alive(pid) {
                outcomes.push(GroupMemberCloseOutcome {
                    browser: member.browser,
                    profile: member.profile.clone(),
                    pid: Some(pid),
                    method: "sigkill".to_string(),
                    success: true,
                    error: None,
                });
                continue;
            }
        }

        outcomes.push(GroupMemberCloseOutcome {
            browser: member.browser,
            profile: member.profile.clone(),
            pid: Some(pid),
            method: "failed".to_string(),
            success: false,
            error: Some("Browser process did not terminate (try --force)".to_string()),
        });
    }

    Ok(GroupCloseReport {
        group: group_name.to_string(),
        members: outcomes,
    })
}

/// Collect status of a single group.
pub fn status_group(storage: &StorageManager, group_name: &str) -> Result<GroupStatusReport> {
    let group = get_group(storage, group_name)?;
    let mut members = Vec::new();
    let mut running_count = 0;

    let cdp_reg = crate::cdp::load_registry(storage);

    for member in &group.members {
        let profile_dir = storage.profile_dir(member.browser, &member.profile);
        let pid = crate::running::running_pid(&profile_dir).ok().flatten();
        let running = pid.is_some();
        if running {
            running_count += 1;
        }

        let proxy_str = match &member.proxy {
            Some(p) => Some(p.to_string()),
            None => storage
                .proxy_for(member.browser, &member.profile)
                .map(|p| p.to_string()),
        };

        let cdp_key = format!("{}:{}", member.browser, member.profile);
        let cdp_endpoint = cdp_reg.get(&cdp_key).and_then(|e| {
            if crate::cdp::port_alive(e.port) {
                Some(e.ws_url())
            } else {
                None
            }
        });

        members.push(GroupMemberStatus {
            browser: member.browser,
            profile: member.profile.clone(),
            running,
            pid,
            proxy: proxy_str,
            cdp_endpoint,
            urls: member.urls.clone(),
        });
    }

    Ok(GroupStatusReport {
        name: group.name,
        description: group.description,
        member_count: members.len(),
        running_count,
        members,
    })
}

/// Collect status of all groups.
pub fn status_all_groups(storage: &StorageManager) -> Result<Vec<GroupStatusReport>> {
    let groups = list_groups(storage)?;
    let mut reports = Vec::new();
    for g in groups {
        reports.push(status_group(storage, &g.name)?);
    }
    Ok(reports)
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
    fn group_model_and_atomic_persistence_roundtrip() {
        let (_tmp, storage) = test_storage();

        let initial = load_groups(&storage).unwrap();
        assert!(initial.groups.is_empty());

        let g1 = create_group(&storage, "dev-team", Some("Engineering workspace")).unwrap();
        assert_eq!(g1.name, "dev-team");
        assert_eq!(g1.description.as_deref(), Some("Engineering workspace"));
        assert!(g1.members.is_empty());

        assert!(create_group(&storage, "dev-team", None).is_err());

        storage
            .create_profile("work", None, Browser::Chrome)
            .unwrap();
        storage
            .create_profile("social", None, Browser::Edge)
            .unwrap();

        let updated = add_member(
            &storage,
            "dev-team",
            Browser::Chrome,
            "work",
            vec!["https://github.com".into()],
            None,
        )
        .unwrap();
        assert_eq!(updated.members.len(), 1);

        let proxy = ProxyConfig::parse("socks5://127.0.0.1:1080", None).unwrap();
        let updated = add_member(
            &storage,
            "dev-team",
            Browser::Edge,
            "social",
            vec!["https://teams.microsoft.com".into()],
            Some(proxy.clone()),
        )
        .unwrap();
        assert_eq!(updated.members.len(), 2);
        assert_eq!(updated.members[1].proxy, Some(proxy));

        let loaded = get_group(&storage, "dev-team").unwrap();
        assert_eq!(loaded.members.len(), 2);
        assert_eq!(loaded.members[0].profile, "work");
        assert_eq!(loaded.members[1].profile, "social");

        let status = status_group(&storage, "dev-team").unwrap();
        assert_eq!(status.name, "dev-team");
        assert_eq!(status.member_count, 2);
        assert_eq!(status.running_count, 0);

        let after_remove = remove_member(&storage, "dev-team", Browser::Chrome, "work").unwrap();
        assert_eq!(after_remove.members.len(), 1);
        assert_eq!(after_remove.members[0].profile, "social");

        delete_group(&storage, "dev-team").unwrap();
        assert!(get_group(&storage, "dev-team").is_err());
        assert!(list_groups(&storage).unwrap().is_empty());
    }

    #[test]
    fn group_save_is_atomic_and_leaves_no_temp_file() {
        let (_tmp, storage) = test_storage();
        create_group(&storage, "test-group", None).unwrap();

        assert!(storage.groups_file().exists());
        assert!(!storage.groups_file().with_extension("tmp").exists());
    }

    #[test]
    fn close_group_reports_not_running_for_inactive_members() {
        let (_tmp, storage) = test_storage();
        storage.create_profile("p1", None, Browser::Chrome).unwrap();
        create_group(&storage, "g1", None).unwrap();
        add_member(&storage, "g1", Browser::Chrome, "p1", vec![], None).unwrap();

        let report = close_group(&storage, "g1", false).unwrap();
        assert_eq!(report.members.len(), 1);
        assert_eq!(report.members[0].method, "not_running");
        assert!(report.members[0].success);
    }
}
