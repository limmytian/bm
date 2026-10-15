use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub name: String,
    pub browser: Browser,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    pub profiles: HashMap<String, ProfileMetadata>,
}

pub struct StorageManager {
    base_dir: PathBuf,
}

impl StorageManager {
    /// Initialize StorageManager using ~/.bm as base directory.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            BmError::Other("Unable to resolve home directory".into())
        })?;
        let base_dir = home.join(".bm");
        Ok(Self { base_dir })
    }

    /// Custom base directory constructor (useful for testing).
    #[allow(dead_code)]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
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

    pub fn metadata_file(&self) -> PathBuf {
        self.base_dir.join("metadata.json")
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
    pub fn load_registry(&self) -> Result<Registry> {
        let file = self.metadata_file();
        if !file.exists() {
            return Ok(Registry::default());
        }
        let content = fs::read_to_string(&file)?;
        let registry: Registry = serde_json::from_str(&content).unwrap_or_default();
        Ok(registry)
    }

    /// Save metadata registry to ~/.bm/metadata.json.
    pub fn save_registry(&self, registry: &Registry) -> Result<()> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
        }
        let file = self.metadata_file();
        let content = serde_json::to_string_pretty(registry)?;
        fs::write(file, content)?;
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
        };

        let mut reg = self.load_registry()?;
        reg.profiles
            .insert(Self::registry_key(browser, name), meta.clone());
        self.save_registry(&reg)?;

        Ok(meta)
    }

    /// Get the currently active profile name by inspecting the browser's default user data symlink.
    pub fn get_current_active(&self, browser: Browser) -> Result<Option<String>> {
        let default_dir = browser.default_data_dir()?;
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
            return Ok(Some(format!(
                "external ({})",
                canonical_target.display()
            )));
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
                };
                reg.profiles.insert(key, m.clone());
                m
            }
        };

        self.save_registry(&reg)?;
        Ok(meta)
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

        let default_dir = browser.default_data_dir()?;
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

        Ok(backup_created)
    }
}
