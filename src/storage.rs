use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub name: String,
    pub browser: Browser,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Optional per-profile egress proxy. Absent in registries created
    /// before proxy support; `#[serde(default)]` keeps those loadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

/// Schema version of `~/.bm/metadata.json`. Version 1 is the initial
/// versioned layout (identical to the pre-versioning format, plus this
/// field). Files written before versioning carry no field and load as
/// version 1 via `#[serde(default)]`; the forward-migration ladder in
/// `migrate_registry` upgrades older versions on load.
pub const METADATA_SCHEMA_VERSION: u32 = 1;

fn current_schema_version() -> u32 {
    METADATA_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// Layout version of this registry file. Absent in pre-versioning
    /// files, which are layout-compatible with version 1.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    pub profiles: HashMap<String, ProfileMetadata>,
}

/// Per-browser switch history persisted in `~/.bm/state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSwitchState {
    /// Profile active before the current one — the `bm switch -` target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_switched_at: Option<DateTime<Utc>>,
}

/// `~/.bm/state.json` shape: keyed by browser name.
pub type SwitchState = HashMap<String, BrowserSwitchState>;

/// Render a coarse relative time ("just now", "5m ago", "3h ago", "12d ago").
pub fn relative_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

pub struct StorageManager {
    base_dir: PathBuf,
    /// Test seam: overrides the browser's default user-data dir (the global
    /// symlink location) so active-profile logic is testable without ever
    /// touching the real browser directories.
    default_dir_override: Option<PathBuf>,
}

impl StorageManager {
    /// Initialize StorageManager using ~/.bm as base directory.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| BmError::Other("Unable to resolve home directory".into()))?;
        let base_dir = home.join(".bm");
        Ok(Self {
            base_dir,
            default_dir_override: None,
        })
    }

    /// Custom base directory constructor (useful for testing).
    #[allow(dead_code)]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            default_dir_override: None,
        }
    }

    /// Route the default-data-dir resolution (global symlink location) at a
    /// custom path; used by tests.
    #[allow(dead_code)]
    pub fn with_default_dir_override(mut self, dir: PathBuf) -> Self {
        self.default_dir_override = Some(dir);
        self
    }

    /// The browser's default user-data dir, honoring the test override.
    pub(crate) fn default_data_dir(&self, browser: Browser) -> Result<PathBuf> {
        match &self.default_dir_override {
            Some(dir) => Ok(dir.clone()),
            None => browser.default_data_dir(),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn profiles_dir(&self, browser: Browser) -> PathBuf {
        self.base_dir.join("profiles").join(browser.to_string())
    }

    pub fn profile_dir(&self, browser: Browser, name: &str) -> PathBuf {
        self.profiles_dir(browser).join(name)
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.base_dir.join("templates")
    }

    pub fn template_dir(&self, name: &str) -> PathBuf {
        self.templates_dir().join(name)
    }

    pub fn temp_base_dir(&self) -> PathBuf {
        self.base_dir.join("temp")
    }

    pub fn temp_dir(&self, id: &str) -> PathBuf {
        self.temp_base_dir().join(id)
    }

    pub fn metadata_file(&self) -> PathBuf {
        self.base_dir.join("metadata.json")
    }

    pub fn groups_file(&self) -> PathBuf {
        self.base_dir.join("groups.json")
    }

    pub fn leases_file(&self) -> PathBuf {
        self.base_dir.join("leases.json")
    }

    pub fn state_file(&self) -> PathBuf {
        self.base_dir.join("state.json")
    }

    /// Load the switch-state map; missing or corrupt files read as empty.
    pub fn load_switch_state(&self) -> SwitchState {
        let file = self.state_file();
        if !file.exists() {
            return SwitchState::default();
        }
        fs::read_to_string(&file)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Record a completed switch for `browser`: the previously active profile
    /// becomes the `bm switch -` target. Written atomically (temp + rename).
    pub fn record_switch(&self, browser: Browser, previous: Option<String>) -> Result<()> {
        let mut state = self.load_switch_state();
        state.insert(
            browser.to_string(),
            BrowserSwitchState {
                last: previous,
                last_switched_at: Some(Utc::now()),
            },
        );
        self.save_switch_state(&state)
    }

    fn save_switch_state(&self, state: &SwitchState) -> Result<()> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
        }
        let tmp = self.base_dir.join("state.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
        fs::rename(&tmp, self.state_file())?;
        Ok(())
    }

    /// The profile that was active before the current one (`bm switch -`).
    pub fn previous_profile(&self, browser: Browser) -> Option<String> {
        self.load_switch_state()
            .get(&browser.to_string())?
            .last
            .clone()
    }

    /// When this browser last switched profiles, for the list column.
    pub fn last_switched_at(&self, browser: Browser) -> Option<DateTime<Utc>> {
        self.load_switch_state()
            .get(&browser.to_string())?
            .last_switched_at
    }

    pub fn ensure_dirs(&self, browser: Browser) -> Result<()> {
        let p = self.profiles_dir(browser);
        if !p.exists() {
            fs::create_dir_all(&p)?;
        }
        Ok(())
    }

    /// Validate profile name strictly.
    pub fn validate_name(name: &str) -> Result<()> {
        if name.is_empty() || name.len() > 64 {
            return Err(BmError::InvalidProfileName(name.to_string()));
        }
        let valid = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(BmError::InvalidProfileName(name.to_string()));
        }
        if name == "." || name == ".." {
            return Err(BmError::InvalidProfileName(name.to_string()));
        }
        Ok(())
    }

    fn registry_key(browser: Browser, name: &str) -> String {
        format!("{}:{}", browser, name)
    }

    /// Load metadata registry from ~/.bm/metadata.json.
    ///
    /// Versioning: the on-disk `schema_version` is checked *before* the full
    /// deserialize so a registry written by a newer bm fails loudly instead
    /// of silently losing fields on the next save (downgrade protection).
    /// Older/absent versions are migrated forward by `migrate_registry`.
    pub fn load_registry(&self) -> Result<Registry> {
        let file = self.metadata_file();
        if !file.exists() {
            return Ok(Registry::default());
        }
        let content = fs::read_to_string(&file)?;
        // Corrupt JSON keeps the historical lenient behavior (empty
        // registry): a hard error here would brick every command after a
        // interrupted write, while the next save simply rebuilds the file.
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Ok(Registry::default());
        };
        let version = raw
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(METADATA_SCHEMA_VERSION as u64) as u32;
        if version > METADATA_SCHEMA_VERSION {
            return Err(BmError::MetadataVersionTooNew {
                found: version,
                supported: METADATA_SCHEMA_VERSION,
            });
        }
        let registry: Registry = serde_json::from_str(&content).unwrap_or_default();
        migrate_registry(registry, version)
    }

    /// Save metadata registry to ~/.bm/metadata.json (atomic: temp + rename,
    /// so an interrupted write can never truncate the file).
    pub fn save_registry(&self, registry: &Registry) -> Result<()> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
        }
        let tmp = self.base_dir.join("metadata.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(registry)?)?;
        fs::rename(&tmp, self.metadata_file())?;
        Ok(())
    }

    /// Create a new profile directory and record its metadata.
    pub fn create_profile(
        &self,
        name: &str,
        description: Option<&str>,
        browser: Browser,
    ) -> Result<ProfileMetadata> {
        Self::validate_name(name)?;
        self.ensure_dirs(browser)?;

        let target_dir = self.profile_dir(browser, name);
        if target_dir.exists() {
            return Err(BmError::ProfileAlreadyExists {
                name: name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        fs::create_dir_all(&target_dir)?;

        let now = Utc::now();
        let meta = ProfileMetadata {
            name: name.to_string(),
            browser,
            description: description.unwrap_or("").trim().to_string(),
            created_at: now,
            updated_at: now,
            proxy: None,
        };

        let mut reg = self.load_registry()?;
        reg.profiles
            .insert(Self::registry_key(browser, name), meta.clone());
        self.save_registry(&reg)?;

        Ok(meta)
    }

    /// Create a new profile, optionally seeding its data directory from a saved template.
    pub fn create_profile_with_template(
        &self,
        name: &str,
        description: Option<&str>,
        browser: Browser,
        template: Option<&str>,
    ) -> Result<(ProfileMetadata, Option<crate::template::TemplateMetadata>)> {
        let tpl_meta = if let Some(tpl) = template {
            let tpl_dir = self.template_dir(tpl);
            let manifest_path = tpl_dir.join("manifest.json");
            if !manifest_path.is_file() {
                return Err(BmError::TemplateNotFound {
                    name: tpl.to_string(),
                });
            }
            let manifest_content = fs::read_to_string(manifest_path)?;
            let meta: crate::template::TemplateMetadata = serde_json::from_str(&manifest_content)?;
            Some(meta)
        } else {
            None
        };

        let effective_desc =
            description.or_else(|| tpl_meta.as_ref().and_then(|m| m.description.as_deref()));

        let meta = self.create_profile(name, effective_desc, browser)?;
        if let Some(tpl) = template {
            let target_dir = self.profile_dir(browser, name);
            crate::template::apply_template(self, tpl, &target_dir)?;
        }
        Ok((meta, tpl_meta))
    }

    /// Get the currently active profile name by inspecting the browser's default user data symlink.
    pub fn get_current_active(&self, browser: Browser) -> Result<Option<String>> {
        let default_dir = self.default_data_dir(browser)?;
        if !default_dir.exists() && fs::symlink_metadata(&default_dir).is_err() {
            return Ok(None);
        }

        let symlink_meta = fs::symlink_metadata(&default_dir)?;
        if symlink_meta.file_type().is_symlink() {
            let target = fs::read_link(&default_dir)?;
            let canonical_target = if target.is_relative() {
                if let Some(parent) = default_dir.parent() {
                    parent.join(&target)
                } else {
                    target
                }
            } else {
                target
            };

            let profiles_prefix = self.profiles_dir(browser);
            if canonical_target.starts_with(&profiles_prefix) {
                if let Some(name) = canonical_target.file_name().and_then(|n| n.to_str()) {
                    return Ok(Some(name.to_string()));
                }
            }
            return Ok(Some(format!("external ({})", canonical_target.display())));
        }

        Ok(None)
    }

    /// List all registered profiles for a specific browser.
    pub fn list_profiles(&self, browser: Browser) -> Result<Vec<(ProfileMetadata, bool)>> {
        self.ensure_dirs(browser)?;
        let reg = self.load_registry()?;
        let active_opt = self.get_current_active(browser)?;

        let mut items = Vec::new();
        let dir = self.profiles_dir(browser);

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        let is_active = active_opt.as_deref() == Some(name);
                        let key = Self::registry_key(browser, name);
                        let meta = match reg.profiles.get(&key) {
                            Some(m) => m.clone(),
                            None => {
                                // If folder exists without metadata, synthesize one
                                let now = Utc::now();
                                ProfileMetadata {
                                    name: name.to_string(),
                                    browser,
                                    description: String::new(),
                                    created_at: now,
                                    updated_at: now,
                                    proxy: None,
                                }
                            }
                        };
                        items.push((meta, is_active));
                    }
                }
            }
        }

        items.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Ok(items)
    }

    /// Update description / note of a profile.
    pub fn describe_profile(
        &self,
        name: &str,
        note: &str,
        browser: Browser,
    ) -> Result<ProfileMetadata> {
        let target_dir = self.profile_dir(browser, name);
        if !target_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        let mut reg = self.load_registry()?;
        let key = Self::registry_key(browser, name);
        let now = Utc::now();

        let meta = match reg.profiles.get_mut(&key) {
            Some(m) => {
                m.description = note.trim().to_string();
                m.updated_at = now;
                m.clone()
            }
            None => {
                let m = ProfileMetadata {
                    name: name.to_string(),
                    browser,
                    description: note.trim().to_string(),
                    created_at: now,
                    updated_at: now,
                    proxy: None,
                };
                reg.profiles.insert(key, m.clone());
                m
            }
        };

        self.save_registry(&reg)?;
        Ok(meta)
    }

    /// Set or clear the per-profile egress proxy.
    /// Passing `None` removes the proxy configuration.
    pub fn set_proxy(
        &self,
        name: &str,
        browser: Browser,
        proxy: Option<ProxyConfig>,
    ) -> Result<ProfileMetadata> {
        let target_dir = self.profile_dir(browser, name);
        if !target_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        let mut reg = self.load_registry()?;
        let key = Self::registry_key(browser, name);
        let now = Utc::now();

        let meta = match reg.profiles.get_mut(&key) {
            Some(m) => {
                m.proxy = proxy;
                m.updated_at = now;
                m.clone()
            }
            None => {
                // Folder exists but has no metadata yet (e.g. created outside
                // bm): synthesize a record, mirroring describe_profile.
                let m = ProfileMetadata {
                    name: name.to_string(),
                    browser,
                    description: String::new(),
                    created_at: now,
                    updated_at: now,
                    proxy,
                };
                reg.profiles.insert(key, m.clone());
                m
            }
        };

        self.save_registry(&reg)?;
        Ok(meta)
    }

    /// Look up the egress proxy configured for a profile, if any. Profiles
    /// that exist only as folders (no metadata) have no proxy.
    pub fn proxy_for(&self, browser: Browser, name: &str) -> Option<ProxyConfig> {
        self.load_registry()
            .ok()?
            .profiles
            .get(&Self::registry_key(browser, name))
            .and_then(|m| m.proxy.clone())
    }

    /// Delete a profile.
    pub fn delete_profile(&self, name: &str, browser: Browser) -> Result<()> {
        let target_dir = self.profile_dir(browser, name);
        if !target_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        if let Ok(Some(active)) = self.get_current_active(browser) {
            if active == name {
                return Err(BmError::ActiveProfileCannotBeDeleted {
                    name: name.to_string(),
                });
            }
        }

        crate::lease::guard_lease(self, browser, name, false)?;

        fs::remove_dir_all(&target_dir)?;

        let mut reg = self.load_registry()?;
        reg.profiles.remove(&Self::registry_key(browser, name));
        let _ = self.save_registry(&reg);

        Ok(())
    }

    /// Switch active profile by safely replacing default data directory with a symlink.
    /// If original browser data directory is not a symlink, automatically back it up as 'original'.
    pub fn switch_profile(
        &self,
        name: &str,
        browser: Browser,
        force: bool,
    ) -> Result<Option<String>> {
        let target_dir = self.profile_dir(browser, name);
        if !target_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        crate::lease::guard_lease(self, browser, name, force)?;

        // Capture the profile being left, for `bm switch -`. Recorded after
        // the switch succeeds; only bm-managed activations count.
        let previous = self
            .get_current_active(browser)?
            .filter(|p| !p.starts_with("external ("));

        if browser.is_running() {
            if force {
                browser.terminate()?;
                std::thread::sleep(std::time::Duration::from_millis(600));
            } else {
                return Err(BmError::BrowserRunning {
                    browser: browser.display_name().to_string(),
                });
            }
        }

        let default_dir = self.default_data_dir(browser)?;
        let mut backup_created = None;

        if fs::symlink_metadata(&default_dir).is_ok() {
            let meta = fs::symlink_metadata(&default_dir)?;
            if meta.file_type().is_symlink() {
                // Safely remove symlink without deleting target folder contents
                fs::remove_file(&default_dir)?;
            } else if meta.is_dir() {
                // Real directory: back up original data so user loses nothing!
                let backup_name = "original";
                let backup_dir = self.profile_dir(browser, backup_name);

                if !backup_dir.exists() {
                    fs::rename(&default_dir, &backup_dir)?;
                    let now = Utc::now();
                    let mut reg = self.load_registry()?;
                    reg.profiles.insert(
                        Self::registry_key(browser, backup_name),
                        ProfileMetadata {
                            name: backup_name.to_string(),
                            browser,
                            description: "Original unmanaged browser data (auto-backed up)"
                                .to_string(),
                            created_at: now,
                            updated_at: now,
                            proxy: None,
                        },
                    );
                    let _ = self.save_registry(&reg);
                    backup_created = Some(backup_name.to_string());
                } else {
                    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
                    let ts_backup_name = format!("original-{}", timestamp);
                    let ts_backup_dir = self.profile_dir(browser, &ts_backup_name);
                    fs::rename(&default_dir, &ts_backup_dir)?;
                    backup_created = Some(ts_backup_name);
                }
            }
        }

        // Ensure parent directory exists
        if let Some(parent) = default_dir.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        // Create atomic symlink
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_dir, &default_dir)?;

        #[cfg(not(unix))]
        return Err(BmError::UnsupportedPlatform(
            "Symlink creation requires Unix-based operating system".into(),
        ));

        // Best-effort: the switch itself succeeded; a state-write hiccup
        // must not fail the command.
        let _ = self.record_switch(browser, previous);

        Ok(backup_created)
    }

    /// Duplicate a profile on the same machine: sessions, cookies and other
    /// valuable state survive; regenerable caches are skipped unless `full`
    /// (see src/cache.rs). The destination registry entry carries the
    /// source's description and egress proxy unless `carry_proxy` is false.
    /// This is intentionally a local-machine operation — no cross-machine
    /// sync.
    pub fn clone_profile(
        &self,
        src: &str,
        dst: &str,
        browser: Browser,
        full: bool,
        carry_proxy: bool,
    ) -> Result<(ProfileMetadata, usize)> {
        if browser.is_running() {
            return Err(BmError::BrowserRunning {
                browser: browser.display_name().to_string(),
            });
        }
        self.clone_profile_unchecked(src, dst, browser, full, carry_proxy)
    }

    /// Clone without the running-browser guard; separate so tests can run
    /// against `is_running`'s process-table dependency.
    pub(crate) fn clone_profile_unchecked(
        &self,
        src: &str,
        dst: &str,
        browser: Browser,
        full: bool,
        carry_proxy: bool,
    ) -> Result<(ProfileMetadata, usize)> {
        Self::validate_name(src)?;
        Self::validate_name(dst)?;

        let src_dir = self.profile_dir(browser, src);
        if !src_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: src.to_string(),
                browser: browser.display_name().to_string(),
            });
        }
        let dst_dir = self.profile_dir(browser, dst);
        if dst_dir.exists() {
            return Err(BmError::ProfileAlreadyExists {
                name: dst.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        self.ensure_dirs(browser)?;
        fs::create_dir_all(&dst_dir)?;
        let copied = copy_tree(&src_dir, &dst_dir, Path::new(""), !full)?;

        let source = self
            .load_registry()?
            .profiles
            .get(&Self::registry_key(browser, src))
            .cloned();
        let now = Utc::now();
        let meta = ProfileMetadata {
            name: dst.to_string(),
            browser,
            description: source
                .as_ref()
                .map(|m| m.description.clone())
                .unwrap_or_default(),
            created_at: now,
            updated_at: now,
            proxy: if carry_proxy {
                source.and_then(|m| m.proxy)
            } else {
                None
            },
        };

        let mut reg = self.load_registry()?;
        reg.profiles
            .insert(Self::registry_key(browser, dst), meta.clone());
        self.save_registry(&reg)?;

        Ok((meta, copied))
    }

    /// Rename a profile: move its directory and re-key its registry entry,
    /// preserving created_at. If the renamed profile is the active one and
    /// the browser is running, refuses — the default-data-dir symlink repoint
    /// must not race a live browser (same guard as switch_profile).
    pub fn rename_profile(
        &self,
        old: &str,
        new: &str,
        browser: Browser,
    ) -> Result<ProfileMetadata> {
        if self.get_current_active(browser)?.as_deref() == Some(old) && browser.is_running() {
            return Err(BmError::BrowserRunning {
                browser: browser.display_name().to_string(),
            });
        }
        self.rename_profile_unchecked(old, new, browser)
    }

    /// Rename without the running-browser guard; separate so tests can run
    /// against `is_running`'s process-table dependency.
    pub(crate) fn rename_profile_unchecked(
        &self,
        old: &str,
        new: &str,
        browser: Browser,
    ) -> Result<ProfileMetadata> {
        Self::validate_name(new)?;

        let src_dir = self.profile_dir(browser, old);
        if !src_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: old.to_string(),
                browser: browser.display_name().to_string(),
            });
        }
        crate::lease::guard_lease(self, browser, old, false)?;
        let dst_dir = self.profile_dir(browser, new);
        if dst_dir.exists() {
            return Err(BmError::ProfileAlreadyExists {
                name: new.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        let was_active = self.get_current_active(browser)?.as_deref() == Some(old);

        fs::rename(&src_dir, &dst_dir)?;

        let mut reg = self.load_registry()?;
        let mut meta = match reg.profiles.remove(&Self::registry_key(browser, old)) {
            Some(m) => m,
            None => ProfileMetadata {
                name: old.to_string(),
                browser,
                description: String::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                proxy: None,
            },
        };
        meta.name = new.to_string();
        meta.updated_at = Utc::now();
        reg.profiles
            .insert(Self::registry_key(browser, new), meta.clone());
        self.save_registry(&reg)?;

        // Keep the global symlink pointing at the renamed dir.
        if was_active {
            let default_dir = self.default_data_dir(browser)?;
            let is_symlink = fs::symlink_metadata(&default_dir)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink {
                fs::remove_file(&default_dir)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&dst_dir, &default_dir)?;
                #[cfg(not(unix))]
                return Err(BmError::UnsupportedPlatform(
                    "Symlink creation requires Unix-based operating system".into(),
                ));
            }
        }

        Ok(meta)
    }
}

/// Forward-migration ladder for the metadata registry: upgrades an
/// in-memory registry from `from_version` to [`METADATA_SCHEMA_VERSION`],
/// one step per version. Each arm transforms the previous layout; the
/// migrated registry is persisted by the caller's next save. Versions above
/// the current one are rejected earlier, in `load_registry`.
fn migrate_registry(mut registry: Registry, from_version: u32) -> Result<Registry> {
    let mut version = from_version;
    while version < METADATA_SCHEMA_VERSION {
        registry.schema_version = version + 1;
        // Future transitions go here, e.g. `1 => { transform v1 → v2 }`.
        // There is no v1 → v2 change yet, so every step is a no-op marker.
        version += 1;
    }
    registry.schema_version = METADATA_SCHEMA_VERSION;
    Ok(registry)
}

/// Recursively copy `src_dir` → `dst_dir`, judging skip decisions against
/// `rel_to_root` (the current directory's path relative to the profile
/// root). Symlinks are recreated pointing at the same target (possibly
/// stale or broken — runtime artifacts like SingletonSocket copy as-is);
/// directory and file permissions are preserved. Sockets/fifos and other
/// non-regular files are skipped. Returns the number of files copied.
fn copy_tree(
    src_dir: &Path,
    dst_dir: &Path,
    rel_to_root: &Path,
    skip_caches: bool,
) -> Result<usize> {
    fs::create_dir_all(dst_dir)?;
    #[cfg(unix)]
    {
        let perms = fs::metadata(src_dir)?.permissions();
        fs::set_permissions(dst_dir, perms)?;
    }

    let mut copied = 0usize;
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let child_rel = rel_to_root.join(entry.file_name());
        if skip_caches && crate::cache::is_regenerable(&child_rel) {
            continue;
        }

        let from = entry.path();
        let to = dst_dir.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            let target = fs::read_link(&from)?;
            std::os::unix::fs::symlink(&target, &to)?;
        } else if file_type.is_dir() {
            copied += copy_tree(&from, &to, &child_rel, skip_caches)?;
        } else if file_type.is_file() {
            // fs::copy preserves permission bits on Unix.
            fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
        (tmp, storage)
    }

    /// Build a realistic minimal profile tree: valuable state plus caches.
    fn seed_profile(
        storage: &StorageManager,
        browser: Browser,
        name: &str,
        proxy: Option<ProxyConfig>,
    ) -> ProfileMetadata {
        let meta = storage
            .create_profile(name, Some("source note"), browser)
            .unwrap();
        if let Some(p) = proxy {
            storage.set_proxy(name, browser, Some(p)).unwrap();
        }
        let root = storage.profile_dir(browser, name);
        fs::create_dir_all(root.join("Default")).unwrap();
        fs::write(root.join("Default/Cookies"), "cookie-db").unwrap();
        fs::write(root.join("Default/Preferences"), "{}").unwrap();
        // Regenerable caches: root-level, nested, and nested two-level.
        fs::create_dir_all(root.join("Cache")).unwrap();
        fs::write(root.join("Cache/data_0"), "cache").unwrap();
        fs::create_dir_all(root.join("Default/GPUCache")).unwrap();
        fs::write(root.join("Default/GPUCache/data_0"), "gpu").unwrap();
        fs::create_dir_all(root.join("Default/Service Worker/CacheStorage")).unwrap();
        fs::write(
            root.join("Default/Service Worker/CacheStorage/https_x"),
            "pwa",
        )
        .unwrap();
        fs::create_dir_all(root.join("component_crx_cache")).unwrap();
        fs::write(root.join("component_crx_cache/x.crx"), "crx").unwrap();
        fs::write(root.join("OptimizationHints"), "hints").unwrap();
        // Symlink inside the tree must be recreated, not followed.
        symlink("Cookies", root.join("Default/link-cookies")).unwrap();
        meta
    }

    #[test]
    fn clone_skips_caches_and_carries_metadata() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        let proxy = ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap();
        let src = seed_profile(&storage, browser, "src", Some(proxy.clone()));

        let (meta, copied) = storage
            .clone_profile_unchecked("src", "dst", browser, false, true)
            .unwrap();

        assert!(copied >= 2, "valuable files must be copied");
        let dst_root = storage.profile_dir(browser, "dst");
        assert_eq!(
            fs::read_to_string(dst_root.join("Default/Cookies")).unwrap(),
            "cookie-db"
        );
        assert!(dst_root.join("Default/Preferences").exists());
        // Symlink recreated as a symlink to the same target.
        let link = dst_root.join("Default/link-cookies");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("Cookies"));
        // Regenerable state absent.
        assert!(!dst_root.join("Cache").exists());
        assert!(!dst_root.join("Default/GPUCache").exists());
        assert!(!dst_root
            .join("Default/Service Worker/CacheStorage")
            .exists());
        assert!(!dst_root.join("component_crx_cache").exists());
        assert!(!dst_root.join("OptimizationHints").exists());

        // Metadata: description and proxy carried, timestamps refreshed.
        assert_eq!(meta.description, "source note");
        assert_eq!(meta.proxy, Some(proxy));
        assert!(meta.created_at >= src.created_at);
        let reg = storage.load_registry().unwrap();
        assert!(reg.profiles.contains_key("chrome:dst"));
    }

    #[test]
    fn clone_full_includes_caches() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed_profile(&storage, browser, "src", None);

        storage
            .clone_profile_unchecked("src", "dst", browser, true, true)
            .unwrap();

        let dst_root = storage.profile_dir(browser, "dst");
        assert!(dst_root.join("Cache/data_0").exists());
        assert!(dst_root.join("Default/GPUCache").exists());
        assert!(dst_root
            .join("Default/Service Worker/CacheStorage/https_x")
            .exists());
        assert!(dst_root.join("OptimizationHints").exists());
    }

    #[test]
    fn clone_no_proxy_strips_proxy() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        let proxy = ProxyConfig::parse("http://p:8080", None).unwrap();
        seed_profile(&storage, browser, "src", Some(proxy));

        let (meta, _) = storage
            .clone_profile_unchecked("src", "dst", browser, false, false)
            .unwrap();
        assert_eq!(meta.proxy, None);
    }

    #[test]
    fn clone_guards() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("src", None, browser).unwrap();
        storage.create_profile("taken", None, browser).unwrap();

        // Invalid names rejected on either side.
        assert!(storage
            .clone_profile_unchecked("bad name", "dst", browser, false, true)
            .is_err());
        assert!(storage
            .clone_profile_unchecked("src", "", browser, false, true)
            .is_err());
        // Missing source.
        assert!(storage
            .clone_profile_unchecked("nope", "dst", browser, false, true)
            .is_err());
        // Existing destination.
        assert!(storage
            .clone_profile_unchecked("src", "taken", browser, false, true)
            .is_err());
        // Cloning onto itself is caught by the exists guard.
        assert!(storage
            .clone_profile_unchecked("src", "src", browser, false, true)
            .is_err());
    }

    #[test]
    fn rename_inactive_profile_moves_dir_and_registry() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        let proxy = ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap();
        let src = storage
            .create_profile("old", Some("keep me"), browser)
            .unwrap();
        storage.set_proxy("old", browser, Some(proxy)).unwrap();
        fs::create_dir_all(storage.profile_dir(browser, "old/Default")).unwrap();
        fs::write(storage.profile_dir(browser, "old/Default/Cookies"), "db").unwrap();

        let meta = storage
            .rename_profile_unchecked("old", "new", browser)
            .unwrap();

        assert!(!storage.profile_dir(browser, "old").exists());
        assert!(storage.profile_dir(browser, "new/Default/Cookies").exists());
        assert_eq!(meta.name, "new");
        assert_eq!(meta.description, "keep me");
        assert_eq!(
            meta.proxy.as_ref().unwrap().server_string(),
            "socks5://10.0.0.1:1080"
        );
        // created_at preserved, updated_at refreshed.
        assert_eq!(meta.created_at, src.created_at);
        assert!(meta.updated_at >= meta.created_at);
        let reg = storage.load_registry().unwrap();
        assert!(!reg.profiles.contains_key("chrome:old"));
        assert!(reg.profiles.contains_key("chrome:new"));
    }

    #[test]
    fn rename_active_profile_repoints_default_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let default_link = tmp.path().join("default-link");
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"))
            .with_default_dir_override(default_link.clone());
        let browser = Browser::Chrome;

        storage.create_profile("a", None, browser).unwrap();
        fs::create_dir_all(storage.profile_dir(browser, "a")).unwrap();
        fs::write(storage.profile_dir(browser, "a/marker"), "x").unwrap();
        symlink(storage.profile_dir(browser, "a"), &default_link).unwrap();

        storage.rename_profile_unchecked("a", "b", browser).unwrap();

        // The global symlink now resolves to the renamed dir.
        assert!(default_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&default_link).unwrap(),
            storage.profile_dir(browser, "b")
        );
        assert_eq!(
            storage.get_current_active(browser).unwrap().as_deref(),
            Some("b")
        );
        assert_eq!(
            fs::read_to_string(default_link.join("marker")).unwrap(),
            "x"
        );
    }

    #[test]
    fn rename_guards() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("old", None, browser).unwrap();
        storage.create_profile("taken", None, browser).unwrap();

        // Invalid new name.
        assert!(storage
            .rename_profile_unchecked("old", "bad name", browser)
            .is_err());
        // Missing source.
        assert!(storage
            .rename_profile_unchecked("nope", "new", browser)
            .is_err());
        // Existing destination.
        assert!(storage
            .rename_profile_unchecked("old", "taken", browser)
            .is_err());
        // Renaming onto itself is caught by the exists guard.
        assert!(storage
            .rename_profile_unchecked("old", "old", browser)
            .is_err());
    }

    #[test]
    fn switch_state_roundtrip() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        assert_eq!(storage.previous_profile(browser), None);
        assert_eq!(storage.last_switched_at(browser), None);

        storage.record_switch(browser, Some("work".into())).unwrap();
        assert_eq!(storage.previous_profile(browser).as_deref(), Some("work"));
        assert!(storage.last_switched_at(browser).is_some());

        // Persists across instances (same base dir).
        let reloaded = StorageManager::with_base_dir(storage.base_dir().to_path_buf());
        assert_eq!(reloaded.previous_profile(browser).as_deref(), Some("work"));

        // A later switch replaces the previous-profile pointer.
        storage
            .record_switch(browser, Some("personal".into()))
            .unwrap();
        assert_eq!(
            storage.previous_profile(browser).as_deref(),
            Some("personal")
        );

        // Per-browser isolation.
        storage
            .record_switch(Browser::Edge, Some("edge-prev".into()))
            .unwrap();
        assert_eq!(
            storage.previous_profile(browser).as_deref(),
            Some("personal")
        );
        assert_eq!(
            storage.previous_profile(Browser::Edge).as_deref(),
            Some("edge-prev")
        );

        // A corrupt state file reads as empty instead of crashing.
        fs::write(storage.state_file(), "not json{").unwrap();
        assert_eq!(storage.previous_profile(browser), None);
        assert_eq!(storage.last_switched_at(browser), None);
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc::now();
        assert_eq!(
            relative_time(now - chrono::Duration::seconds(10), now),
            "just now"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(3), now),
            "3h ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::days(12), now),
            "12d ago"
        );
        // Future timestamps clamp instead of going negative.
        assert_eq!(
            relative_time(now + chrono::Duration::hours(2), now),
            "just now"
        );
    }

    /// A metadata.json written before versioning: no schema_version field.
    const LEGACY_METADATA: &str = r#"{"profiles":{"chrome:old":{"name":"old","browser":"chrome","description":"pre-versioning","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}}"#;

    #[test]
    fn legacy_metadata_without_schema_version_loads() {
        let (_tmp, storage) = setup();
        fs::create_dir_all(storage.base_dir()).unwrap();
        fs::write(storage.metadata_file(), LEGACY_METADATA).unwrap();

        let reg = storage.load_registry().unwrap();
        assert_eq!(reg.schema_version, METADATA_SCHEMA_VERSION);
        assert_eq!(reg.profiles.len(), 1);
        assert_eq!(reg.profiles["chrome:old"].description, "pre-versioning");

        // A round-trip through save persists the versioned form.
        storage.save_registry(&reg).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(storage.metadata_file()).unwrap()).unwrap();
        assert_eq!(raw["schema_version"], METADATA_SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_version_is_rejected_without_modifying_the_file() {
        let (_tmp, storage) = setup();
        fs::create_dir_all(storage.base_dir()).unwrap();
        let future = r#"{"schema_version":99,"profiles":{}}"#;
        fs::write(storage.metadata_file(), future).unwrap();
        let before = fs::read_to_string(storage.metadata_file()).unwrap();

        let err = storage.load_registry().unwrap_err();
        assert!(matches!(
            err,
            BmError::MetadataVersionTooNew {
                found: 99,
                supported: 1
            }
        ));
        assert_eq!(fs::read_to_string(storage.metadata_file()).unwrap(), before);
        // The message must tell the user what happened and what to do.
        assert!(err.to_string().contains("upgrade bm"));
    }

    #[test]
    fn corrupt_metadata_still_reads_as_empty() {
        let (_tmp, storage) = setup();
        fs::create_dir_all(storage.base_dir()).unwrap();
        fs::write(storage.metadata_file(), "not json{").unwrap();
        let reg = storage.load_registry().unwrap();
        assert!(reg.profiles.is_empty());
    }

    #[test]
    fn save_registry_is_atomic_and_leaves_no_temp_file() {
        let (_tmp, storage) = setup();
        storage
            .create_profile("work", None, Browser::Chrome)
            .unwrap();
        let reg = storage.load_registry().unwrap();
        storage.save_registry(&reg).unwrap();

        assert!(storage.metadata_file().exists());
        assert!(!storage.base_dir().join("metadata.json.tmp").exists());
        // Reload from disk picks the entry back up.
        assert!(storage
            .load_registry()
            .unwrap()
            .profiles
            .contains_key("chrome:work"));
    }

    #[test]
    fn create_profile_with_template_validates_and_seeds() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;

        // Non-existent template fails before creating profile
        let err = storage
            .create_profile_with_template("dev1", None, browser, Some("nonexistent"))
            .unwrap_err();
        assert!(matches!(err, BmError::TemplateNotFound { .. }));
        assert!(!storage.profile_dir(browser, "dev1").exists());

        // Create a template
        let tpl_dir = storage.template_dir("base-tpl");
        fs::create_dir_all(tpl_dir.join("data").join("Default")).unwrap();
        fs::write(
            tpl_dir.join("manifest.json"),
            serde_json::to_vec(&crate::template::TemplateMetadata {
                manifest_version: 1,
                name: "base-tpl".into(),
                browser,
                source_profile: "src".into(),
                description: Some("Base template".into()),
                created_at: Utc::now(),
                bm_version: "0.0.8".into(),
                components: vec!["Bookmarks".into()],
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(
            tpl_dir.join("data").join("Default").join("Bookmarks"),
            b"bookmarks-content",
        )
        .unwrap();

        // Successful creation with template
        let (meta, tpl_meta) = storage
            .create_profile_with_template("dev1", Some("seeded"), browser, Some("base-tpl"))
            .unwrap();
        assert_eq!(meta.name, "dev1");
        assert_eq!(tpl_meta.unwrap().name, "base-tpl");

        let seeded_bm = storage
            .profile_dir(browser, "dev1")
            .join("Default")
            .join("Bookmarks");
        assert_eq!(fs::read(&seeded_bm).unwrap(), b"bookmarks-content");
    }
}
