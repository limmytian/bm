//! Profile template management (`bm template save/list/delete`).
//!
//! A template bundle captures reusable profile configurations:
//! - Extensions (unpacked/CRX files, rules, state, local/sync extension settings)
//! - Bookmarks and bookmark backups
//! - Browser preferences and root Local State (with all account/sync/login/session
//!   recovery tokens strictly stripped)
//!
//! Cookies, session tokens, login data/credentials, browsing history, and
//! website local storage (IndexedDB, WebSQL, localStorage) are physically
//! excluded with multi-layer verification.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::StorageManager;

pub const TEMPLATE_MANIFEST_VERSION: u32 = 1;

/// Metadata describing a saved profile template bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateMetadata {
    pub manifest_version: u32,
    pub name: String,
    pub browser: Browser,
    pub source_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub bm_version: String,
    pub components: Vec<String>,
}

/// Check if a relative path inside a profile holds forbidden data
/// (cookies, sessions, credentials, history, website local storage, locks, or regenerables).
pub fn is_forbidden_entry(rel_path: &Path) -> bool {
    let rel_str = rel_path.to_string_lossy();

    // Regenerables and caches
    if crate::cache::is_regenerable(rel_path) {
        return true;
    }

    for component in rel_path.components() {
        let name = component.as_os_str().to_string_lossy();

        // Cookies and network session tokens
        if name == "Cookies"
            || name == "Cookies-journal"
            || name == "Cookies-wal"
            || name == "Cookies-shm"
            || name == "Network Persistent State"
            || name.starts_with("Trust Tokens")
            || name == "TransportSecurity"
        {
            return true;
        }

        // Sessions and active tabs
        if name == "Sessions"
            || name == "Session Storage"
            || name == "Current Session"
            || name == "Current Tabs"
            || name == "Last Session"
            || name == "Last Tabs"
        {
            return true;
        }

        // Credentials, saved logins, autofill
        if name == "Login Data"
            || name == "Login Data-journal"
            || name == "Login Data-wal"
            || name == "Login Data-shm"
            || name.starts_with("Login Data For Account")
            || name == "Web Data"
            || name == "Web Data-journal"
            || name == "Web Data-wal"
            || name == "Web Data-shm"
            || name.starts_with("Affiliation Database")
            || name == "Accounts"
        {
            return true;
        }

        // History, top sites, visited links
        if name == "History"
            || name == "History-journal"
            || name == "History-wal"
            || name == "History-shm"
            || name == "History Provider Cache"
            || name.starts_with("Favicons")
            || name.starts_with("Top Sites")
            || name.starts_with("Shortcuts")
            || name == "Visited Links"
        {
            return true;
        }

        // Website HTML5 storage (exclude website storage, keep extension storage)
        if name == "Local Storage"
            || name == "IndexedDB"
            || name == "databases"
            || name == "File System"
            || name == "Service Worker"
            || name == "SharedStorage"
            || name == "Storage"
            || name == "blob_storage"
        {
            return true;
        }

        // Locks and process sockets
        if name == "SingletonLock"
            || name == "SingletonCookie"
            || name == "SingletonSocket"
            || name == "lockfile"
        {
            return true;
        }
    }

    // Direct string matches for safety across nested paths (e.g. Network/Cookies)
    if rel_str.contains("Network/Cookies") || rel_str.contains("Network\\Cookies") {
        return true;
    }

    false
}

/// Recursively strip authentication, sync, and account information from
/// Chromium Preferences or Local State JSON.
pub fn sanitize_preferences_json(value: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = value {
        // Strip direct sensitive keys
        map.remove("account_info");
        map.remove("signin");
        map.remove("sync");
        map.remove("password_manager");
        map.remove("autofill");
        map.remove("oauth2");
        map.remove("identity");
        map.remove("credentials_enable_service");
        map.remove("credentials_enable_autosignin");

        if let Some(serde_json::Value::Object(google)) = map.get_mut("google") {
            if let Some(serde_json::Value::Object(services)) = google.get_mut("services") {
                services.remove("signin");
                services.remove("username");
            }
        }

        if let Some(serde_json::Value::Object(profile)) = map.get_mut("profile") {
            profile.remove("info_cache");
            profile.remove("gaia_id");
            profile.remove("account_id");
            profile.remove("user_name");
        }

        if let Some(serde_json::Value::Object(session)) = map.get_mut("session") {
            session.remove("restore_on_startup");
            session.remove("startup_urls");
        }

        for child in map.values_mut() {
            sanitize_preferences_json(child);
        }
    } else if let serde_json::Value::Array(arr) = value {
        for item in arr.iter_mut() {
            sanitize_preferences_json(item);
        }
    }
}

/// Recursively copy a directory while enforcing physical exclusion of forbidden entries.
fn copy_clean_dir(src: &Path, dst: &Path, rel_root: &Path) -> Result<usize> {
    if is_forbidden_entry(rel_root) {
        return Ok(0);
    }

    fs::create_dir_all(dst)?;
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(src) {
        let perms = meta.permissions();
        let _ = fs::set_permissions(dst, perms);
    }

    let mut copied = 0usize;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let child_rel = rel_root.join(&name);

        if is_forbidden_entry(&child_rel) {
            continue;
        }

        let from = entry.path();
        let to = dst.join(&name);
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copied += copy_clean_dir(&from, &to, &child_rel)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Recursively audit a directory to ensure absolutely NO forbidden data exists.
pub fn audit_strictly_clean(dir: &Path, rel_root: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let child_rel = rel_root.join(&name);

        if is_forbidden_entry(&child_rel) {
            return Err(BmError::Other(format!(
                "Sanitization audit failure: forbidden entry detected in template: {}",
                child_rel.display()
            )));
        }

        let path = entry.path();
        if entry.file_type()?.is_dir() {
            audit_strictly_clean(&path, &child_rel)?;
        }
    }
    Ok(())
}

/// Save a profile as a reusable template bundle under `~/.bm/templates/<name>`.
pub fn save_template(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    profile: &str,
    note: Option<&str>,
    force: bool,
) -> Result<TemplateMetadata> {
    StorageManager::validate_name(name)?;
    StorageManager::validate_name(profile)?;

    let src_dir = storage.profile_dir(browser, profile);
    if !src_dir.exists() {
        return Err(BmError::ProfileNotFound {
            name: profile.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    let tpl_dir = storage.template_dir(name);
    if tpl_dir.exists() {
        if !force {
            return Err(BmError::TemplateAlreadyExists {
                name: name.to_string(),
            });
        }
        fs::remove_dir_all(&tpl_dir)?;
    }

    let data_dir = tpl_dir.join("data");
    fs::create_dir_all(&data_dir)?;

    let mut components = Vec::new();

    // 1. Root Local State
    let local_state_src = src_dir.join("Local State");
    if local_state_src.is_file() {
        if let Ok(content) = fs::read_to_string(&local_state_src) {
            if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
                sanitize_preferences_json(&mut json);
                if let Ok(clean) = serde_json::to_vec_pretty(&json) {
                    fs::write(data_dir.join("Local State"), clean)?;
                    components.push("Local State".to_string());
                }
            }
        }
    }

    // 2. Discover profile directories inside src_dir (Default, Profile 1, etc.)
    let mut profile_subdirs = Vec::new();
    if src_dir.join("Default").is_dir() {
        profile_subdirs.push("Default".to_string());
    }
    if let Ok(entries) = fs::read_dir(&src_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name.starts_with("Profile ")
                && entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            {
                profile_subdirs.push(file_name);
            }
        }
    }
    // If no subdirectory exists, check if src_dir itself has profile files
    if profile_subdirs.is_empty() {
        profile_subdirs.push("Default".to_string());
    }

    let mut found_bookmarks = false;
    let mut found_preferences = false;
    let mut found_extensions = false;

    for subdir_name in &profile_subdirs {
        let sub_src = if src_dir.join(subdir_name).is_dir() {
            src_dir.join(subdir_name)
        } else {
            src_dir.clone()
        };
        let sub_dst = data_dir.join(subdir_name);

        // Bookmarks
        for bm_file in ["Bookmarks", "Bookmarks.bak"] {
            let from = sub_src.join(bm_file);
            if from.is_file() {
                fs::create_dir_all(&sub_dst)?;
                fs::copy(&from, sub_dst.join(bm_file))?;
                found_bookmarks = true;
            }
        }

        // Preferences & Secure Preferences
        for pref_file in ["Preferences", "Secure Preferences"] {
            let from = sub_src.join(pref_file);
            if from.is_file() {
                if let Ok(content) = fs::read_to_string(&from) {
                    if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
                        sanitize_preferences_json(&mut json);
                        if let Ok(clean) = serde_json::to_vec_pretty(&json) {
                            fs::create_dir_all(&sub_dst)?;
                            fs::write(sub_dst.join(pref_file), clean)?;
                            found_preferences = true;
                        }
                    }
                }
            }
        }

        // Extensions and extension configurations
        for ext_dir in [
            "Extensions",
            "Extension Rules",
            "Extension State",
            "Local Extension Settings",
            "Sync Extension Settings",
            "Managed Extension Settings",
        ] {
            let from = sub_src.join(ext_dir);
            if from.is_dir() {
                let to = sub_dst.join(ext_dir);
                let rel = Path::new(subdir_name).join(ext_dir);
                let copied = copy_clean_dir(&from, &to, &rel)?;
                if copied > 0 {
                    found_extensions = true;
                }
            }
        }
    }

    if found_bookmarks {
        components.push("Bookmarks".to_string());
    }
    if found_preferences {
        components.push("Preferences".to_string());
    }
    if found_extensions {
        components.push("Extensions".to_string());
    }

    // Defense-in-depth: audit the extracted data directory
    if let Err(e) = audit_strictly_clean(&data_dir, Path::new("")) {
        let _ = fs::remove_dir_all(&tpl_dir);
        return Err(e);
    }

    let manifest = TemplateMetadata {
        manifest_version: TEMPLATE_MANIFEST_VERSION,
        name: name.to_string(),
        browser,
        source_profile: profile.to_string(),
        description: note.map(str::to_string),
        created_at: Utc::now(),
        bm_version: env!("CARGO_PKG_VERSION").to_string(),
        components,
    };

    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(tpl_dir.join("manifest.json"), manifest_bytes)?;

    Ok(manifest)
}

/// List all saved profile templates, optionally filtered by browser.
pub fn list_templates(
    storage: &StorageManager,
    browser_filter: Option<Browser>,
) -> Result<Vec<TemplateMetadata>> {
    let tpls_dir = storage.templates_dir();
    if !tpls_dir.exists() {
        return Ok(Vec::new());
    }

    let mut templates = Vec::new();
    for entry in fs::read_dir(&tpls_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = entry.path().join("manifest.json");
        if manifest_path.is_file() {
            if let Ok(content) = fs::read_to_string(&manifest_path) {
                if let Ok(meta) = serde_json::from_str::<TemplateMetadata>(&content) {
                    if let Some(b) = browser_filter {
                        if meta.browser != b {
                            continue;
                        }
                    }
                    templates.push(meta);
                }
            }
        }
    }

    templates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(templates)
}

/// Delete a template bundle by name.
pub fn delete_template(storage: &StorageManager, name: &str) -> Result<()> {
    StorageManager::validate_name(name)?;
    let tpl_dir = storage.template_dir(name);
    if !tpl_dir.exists() {
        return Err(BmError::TemplateNotFound {
            name: name.to_string(),
        });
    }

    fs::remove_dir_all(&tpl_dir)?;
    Ok(())
}

/// Seed an existing target directory with data from a saved template bundle.
pub fn apply_template(
    storage: &StorageManager,
    template_name: &str,
    target_profile_dir: &Path,
) -> Result<TemplateMetadata> {
    let tpl_dir = storage.template_dir(template_name);
    let manifest_path = tpl_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(BmError::TemplateNotFound {
            name: template_name.to_string(),
        });
    }

    let content = fs::read_to_string(&manifest_path)?;
    let meta: TemplateMetadata = serde_json::from_str(&content)?;

    let data_dir = tpl_dir.join("data");
    if data_dir.exists() {
        copy_clean_dir(&data_dir, target_profile_dir, Path::new(""))?;
    }

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_storage() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
        (tmp, storage)
    }

    #[test]
    fn forbidden_entry_detector_catches_all_sensitive_files() {
        assert!(is_forbidden_entry(Path::new("Default/Cookies")));
        assert!(is_forbidden_entry(Path::new("Default/Cookies-journal")));
        assert!(is_forbidden_entry(Path::new("Default/Network/Cookies")));
        assert!(is_forbidden_entry(Path::new("Default/Sessions")));
        assert!(is_forbidden_entry(Path::new("Default/Current Session")));
        assert!(is_forbidden_entry(Path::new("Default/Last Tabs")));
        assert!(is_forbidden_entry(Path::new("Default/Login Data")));
        assert!(is_forbidden_entry(Path::new("Default/Web Data")));
        assert!(is_forbidden_entry(Path::new("Default/History")));
        assert!(is_forbidden_entry(Path::new("Default/Local Storage")));
        assert!(is_forbidden_entry(Path::new("Default/IndexedDB")));
        assert!(is_forbidden_entry(Path::new("SingletonLock")));
        assert!(is_forbidden_entry(Path::new("Default/Cache")));

        // Valid non-forbidden entries
        assert!(!is_forbidden_entry(Path::new("Default/Bookmarks")));
        assert!(!is_forbidden_entry(Path::new("Default/Preferences")));
        assert!(!is_forbidden_entry(Path::new("Default/Extensions")));
        assert!(!is_forbidden_entry(Path::new(
            "Default/Local Extension Settings"
        )));
        assert!(!is_forbidden_entry(Path::new("Local State")));
    }

    #[test]
    fn preferences_sanitization_removes_tokens_and_signin() {
        let mut pref = serde_json::json!({
            "account_info": [{"gaia_id": "12345"}],
            "signin": {"allowed": true},
            "google": {
                "services": {
                    "signin": true,
                    "username": "alice@gmail.com"
                }
            },
            "profile": {
                "info_cache": {"dummy": 1},
                "gaia_id": "12345"
            },
            "password_manager": {"os_password_blank": false},
            "extensions": {
                "settings": {
                    "cjpalhdlnbpafiamejdnhcphjbkeiagm": {
                        "active_bit": true
                    }
                }
            }
        });

        sanitize_preferences_json(&mut pref);

        assert!(pref.get("account_info").is_none());
        assert!(pref.get("signin").is_none());
        assert!(pref.get("password_manager").is_none());
        assert!(pref["google"]["services"].get("signin").is_none());
        assert!(pref["google"]["services"].get("username").is_none());
        assert!(pref["profile"].get("gaia_id").is_none());
        assert!(pref["profile"].get("info_cache").is_none());
        // Extension settings preserved
        assert_eq!(
            pref["extensions"]["settings"]["cjpalhdlnbpafiamejdnhcphjbkeiagm"]["active_bit"],
            true
        );
    }

    #[test]
    fn save_list_delete_template_roundtrip_with_strict_physical_stripping() {
        let (_tmp, storage) = create_test_storage();
        let browser = Browser::Chrome;

        // Create source profile
        storage
            .create_profile("source-profile", Some("Source test profile"), browser)
            .unwrap();
        let src_dir = storage.profile_dir(browser, "source-profile");
        let default_dir = src_dir.join("Default");
        fs::create_dir_all(&default_dir).unwrap();

        // Populate reusable items
        fs::write(
            src_dir.join("Local State"),
            r#"{"browser":{"enabled_labs":true}}"#,
        )
        .unwrap();
        fs::write(
            default_dir.join("Bookmarks"),
            r#"{"roots":{"bookmark_bar":{"children":[]}}}"#,
        )
        .unwrap();
        fs::write(
            default_dir.join("Preferences"),
            r#"{"signin":{"token":"secret123"},"homepage":"https://example.com"}"#,
        )
        .unwrap();

        let ext_dir = default_dir.join("Extensions").join("ext123").join("1.0.0");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("manifest.json"), r#"{"name":"test-ext"}"#).unwrap();

        // Populate sensitive items that MUST be stripped
        fs::write(default_dir.join("Cookies"), b"sqlite-cookie-data").unwrap();
        fs::write(default_dir.join("Login Data"), b"sqlite-login-data").unwrap();
        fs::write(default_dir.join("History"), b"sqlite-history-data").unwrap();
        fs::write(default_dir.join("Web Data"), b"sqlite-web-data").unwrap();
        fs::create_dir_all(default_dir.join("Sessions")).unwrap();
        fs::write(
            default_dir.join("Sessions").join("Tabs_001"),
            b"session-tabs",
        )
        .unwrap();
        fs::create_dir_all(default_dir.join("Local Storage").join("leveldb")).unwrap();
        fs::write(
            default_dir
                .join("Local Storage")
                .join("leveldb")
                .join("000001.log"),
            b"localstorage",
        )
        .unwrap();
        fs::create_dir_all(default_dir.join("IndexedDB")).unwrap();
        fs::write(default_dir.join("IndexedDB").join("test.db"), b"indexeddb").unwrap();

        // 1. Save template
        let meta = save_template(
            &storage,
            browser,
            "dev-template",
            "source-profile",
            Some("Standard dev environment"),
            false,
        )
        .unwrap();

        assert_eq!(meta.name, "dev-template");
        assert_eq!(meta.browser, Browser::Chrome);
        assert_eq!(meta.source_profile, "source-profile");
        assert!(meta.components.contains(&"Bookmarks".to_string()));
        assert!(meta.components.contains(&"Preferences".to_string()));
        assert!(meta.components.contains(&"Extensions".to_string()));
        assert!(meta.components.contains(&"Local State".to_string()));

        let tpl_dir = storage.template_dir("dev-template");
        let data_dir = tpl_dir.join("data");

        // Verify preserved items
        assert!(data_dir.join("Local State").exists());
        assert!(data_dir.join("Default").join("Bookmarks").exists());
        assert!(data_dir.join("Default").join("Preferences").exists());
        assert!(data_dir
            .join("Default")
            .join("Extensions")
            .join("ext123")
            .join("1.0.0")
            .join("manifest.json")
            .exists());

        // Verify sanitized preferences
        let saved_pref: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(data_dir.join("Default").join("Preferences")).unwrap(),
        )
        .unwrap();
        assert!(saved_pref.get("signin").is_none());
        assert_eq!(saved_pref["homepage"], "https://example.com");

        // Verify STRICT PHYSICAL STRIPPING of all sensitive data
        assert!(
            !data_dir.join("Default").join("Cookies").exists(),
            "Cookies MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("Login Data").exists(),
            "Login Data MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("History").exists(),
            "History MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("Web Data").exists(),
            "Web Data MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("Sessions").exists(),
            "Sessions MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("Local Storage").exists(),
            "Local Storage MUST be stripped"
        );
        assert!(
            !data_dir.join("Default").join("IndexedDB").exists(),
            "IndexedDB MUST be stripped"
        );

        // 2. Refuses overwrite without force
        let err = save_template(
            &storage,
            browser,
            "dev-template",
            "source-profile",
            None,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, BmError::TemplateAlreadyExists { .. }));

        // 3. List templates
        let list = list_templates(&storage, None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "dev-template");

        // 4. Delete template
        delete_template(&storage, "dev-template").unwrap();
        assert!(!tpl_dir.exists());

        let list_after = list_templates(&storage, None).unwrap();
        assert!(list_after.is_empty());
    }
}
