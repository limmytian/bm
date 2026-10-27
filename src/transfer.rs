//! Cross-machine configuration sharing (`bm export` / `bm import`).
//!
//! An export bundle is a small, human-inspectable JSON file carrying ONLY
//! the profile's configuration: name, description, browser kind, proxy,
//! timestamps. It deliberately contains **no session data** — no cookies,
//! logins, history, or extensions. Teams use it to standardize environments
//! ("this is what our `work` profile looks like"); moving actual data
//! between machines stays the job of `bm backup` + `bm restore` (a full
//! tar.zst snapshot). This boundary is the whole point of the feature and
//! is spelled out in the README.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::proxy::ProxyConfig;
use crate::storage::{ProfileMetadata, StorageManager};

pub const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// The shareable configuration bundle. Field set is intentionally minimal;
/// adding fields is a schema-version bump with a migration on import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub bundle_schema_version: u32,
    pub name: String,
    pub description: String,
    pub browser: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
    pub created_at: DateTime<Utc>,
    pub exported_at: DateTime<Utc>,
    pub bm_version: String,
}

fn exports_dir(storage: &StorageManager) -> PathBuf {
    storage.base_dir().join("exports")
}

/// Export a profile's configuration bundle. Default output:
/// `~/.bm/exports/bm-export-<profile>-<ts>.json`; an explicit `file`
/// overrides. The profile must exist; its data is never read beyond the
/// registry entry.
pub fn export_profile(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    file: Option<&Path>,
) -> Result<PathBuf> {
    let (meta, _) = storage
        .list_profiles(browser)?
        .into_iter()
        .find(|(m, _)| m.name == name)
        .ok_or_else(|| BmError::ProfileNotFound {
            name: name.to_string(),
            browser: browser.display_name().to_string(),
        })?;

    let bundle = ExportBundle {
        bundle_schema_version: BUNDLE_SCHEMA_VERSION,
        name: meta.name.clone(),
        description: meta.description,
        browser: meta.browser.to_string(),
        proxy: meta.proxy,
        created_at: meta.created_at,
        exported_at: Utc::now(),
        bm_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let out = match file {
        Some(f) => f.to_path_buf(),
        None => {
            let dir = exports_dir(storage);
            fs::create_dir_all(&dir)?;
            let ts = Utc::now().format("%Y%m%d%H%M%S");
            dir.join(format!("bm-export-{name}-{ts}.json"))
        }
    };
    fs::write(&out, serde_json::to_vec_pretty(&bundle)?)?;
    Ok(out)
}

/// Register a profile from an export bundle on this machine: create the
/// profile dir placeholder (empty — the browser rebuilds its contents) and
/// write the metadata (description + proxy). `rename_to` overrides the
/// bundled name; an existing profile is refused unless `force`, in which
/// case only the registry entry is refreshed (existing dir contents stay).
/// Importing while the browser runs is safe: nothing touches a live data
/// directory.
pub fn import_profile(
    storage: &StorageManager,
    file: &Path,
    rename_to: Option<&str>,
    force: bool,
) -> Result<ProfileMetadata> {
    let content = fs::read_to_string(file)
        .map_err(|e| BmError::Other(format!("cannot read bundle {}: {e}", file.display())))?;
    let bundle: ExportBundle = serde_json::from_str(&content).map_err(|e| {
        BmError::Other(format!("{} is not a bm export bundle: {e}", file.display()))
    })?;
    if bundle.bundle_schema_version > BUNDLE_SCHEMA_VERSION {
        return Err(BmError::Other(format!(
            "bundle {} uses schema version {}, but this bm supports up to {BUNDLE_SCHEMA_VERSION} — upgrade bm",
            file.display(),
            bundle.bundle_schema_version
        )));
    }
    let browser = match bundle.browser.as_str() {
        "chrome" => Browser::Chrome,
        "edge" => Browser::Edge,
        "brave" => Browser::Brave,
        "chromium" => Browser::Chromium,
        other => {
            return Err(BmError::Other(format!(
                "bundle names unsupported browser '{other}'"
            )))
        }
    };

    let name = match rename_to {
        Some(n) => {
            StorageManager::validate_name(n)?;
            n.to_string()
        }
        None => {
            StorageManager::validate_name(&bundle.name)?;
            bundle.name.clone()
        }
    };

    let dir = storage.profile_dir(browser, &name);
    if dir.exists() && !force {
        return Err(BmError::ProfileAlreadyExists {
            name: name.clone(),
            browser: browser.display_name().to_string(),
        });
    }
    storage.ensure_dirs(browser)?;
    fs::create_dir_all(&dir)?;

    let mut reg = storage.load_registry()?;
    let now = Utc::now();
    let meta = ProfileMetadata {
        name: name.clone(),
        browser,
        description: bundle.description,
        created_at: bundle.created_at,
        updated_at: now,
        proxy: bundle.proxy,
    };
    reg.profiles.insert(format!("{browser}:{name}"), meta.clone());
    storage.save_registry(&reg)?;

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        (tmp, StorageManager::with_base_dir(base))
    }

    #[test]
    fn export_writes_a_config_only_bundle() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("work", Some("team standard"), browser).unwrap();
        storage
            .set_proxy("work", browser, Some(ProxyConfig::parse("socks5://10.0.0.1:1080", Some("localhost")).unwrap()))
            .unwrap();
        // Session data on disk that must NOT travel.
        let root = storage.profile_dir(browser, "work");
        fs::create_dir_all(root.join("Default")).unwrap();
        fs::write(root.join("Default/Cookies"), "secret-cookie-db").unwrap();

        let out = export_profile(&storage, browser, "work", None).unwrap();
        let raw = fs::read_to_string(&out).unwrap();
        let bundle: ExportBundle = serde_json::from_str(&raw).unwrap();

        assert_eq!(bundle.name, "work");
        assert_eq!(bundle.description, "team standard");
        assert_eq!(bundle.browser, "chrome");
        assert_eq!(
            bundle.proxy.unwrap().server_string(),
            "socks5://10.0.0.1:1080"
        );
        assert_eq!(bundle.bundle_schema_version, BUNDLE_SCHEMA_VERSION);
        // Human-inspectable and free of anything session-shaped. The proxy
        // serializes structurally (scheme/host/port), not as a server string.
        assert!(raw.contains("\"socks5\""));
        assert!(raw.contains("10.0.0.1"));
        assert!(!raw.contains("secret-cookie-db"));
        assert!(!raw.contains("Cookies"));
        // Default location: ~/.bm/exports/bm-export-<name>-<ts>.json
        assert!(out.starts_with(storage.base_dir().join("exports")));
        assert!(out.file_name().unwrap().to_string_lossy().starts_with("bm-export-work-"));
    }

    #[test]
    fn export_explicit_path_and_missing_profile() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("work", None, browser).unwrap();

        let custom = storage.base_dir().join("custom.json");
        let out = export_profile(&storage, browser, "work", Some(&custom)).unwrap();
        assert_eq!(out, custom);

        assert!(export_profile(&storage, browser, "ghost", None).is_err());
    }

    #[test]
    fn import_registers_placeholder_and_metadata() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("work", Some("team standard"), browser).unwrap();
        storage
            .set_proxy("work", browser, Some(ProxyConfig::parse("http://p:8080", None).unwrap()))
            .unwrap();
        let bundle_path = export_profile(&storage, browser, "work", None).unwrap();

        // Fresh machine (a second storage) receives the bundle cleanly.
        let meta = import_profile(&storage, &bundle_path, Some("imported"), false).unwrap();
        assert_eq!(meta.name, "imported");
        assert_eq!(meta.description, "team standard");
        assert_eq!(meta.proxy.unwrap().server_string(), "http://p:8080");
        // Placeholder dir exists (empty); browser rebuilds the rest.
        assert!(storage.profile_dir(browser, "imported").is_dir());

        // Re-importing over an existing profile refuses without --force...
        assert!(import_profile(&storage, &bundle_path, None, false).is_err());
        // ...and with --force it refreshes metadata but keeps dir contents.
        let marker = storage.profile_dir(browser, "work").join("Default");
        fs::create_dir_all(&marker).unwrap();
        fs::write(marker.join("Cookies"), "real-data").unwrap();
        import_profile(&storage, &bundle_path, None, true).unwrap();
        assert_eq!(
            fs::read_to_string(marker.join("Cookies")).unwrap(),
            "real-data",
            "force re-registers metadata but keeps existing data"
        );
    }

    #[test]
    fn import_renames_and_rejects_bad_bundles() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        storage.create_profile("work", None, browser).unwrap();
        let bundle_path = export_profile(&storage, browser, "work", None).unwrap();

        let meta = import_profile(&storage, &bundle_path, Some("work-copy"), false).unwrap();
        assert_eq!(meta.name, "work-copy");
        assert!(storage.profile_dir(browser, "work-copy").is_dir());

        // Not a bundle.
        let junk = storage.base_dir().join("junk.json");
        fs::write(&junk, "{ not json").unwrap();
        let err = import_profile(&storage, &junk, None, false).unwrap_err();
        assert!(err.to_string().contains("not a bm export bundle"), "got: {err}");

        // Future schema version.
        let future = storage.base_dir().join("future.json");
        fs::write(
            &future,
            r#"{"bundle_schema_version":99,"name":"x","description":"","browser":"chrome","created_at":"2026-01-01T00:00:00Z","exported_at":"2026-01-01T00:00:00Z","bm_version":"9.9.9"}"#,
        )
        .unwrap();
        let err = import_profile(&storage, &future, None, false).unwrap_err();
        assert!(err.to_string().contains("upgrade bm"), "got: {err}");

        // Unsupported browser.
        let safari = storage.base_dir().join("safari.json");
        fs::write(
            &safari,
            r#"{"bundle_schema_version":1,"name":"x","description":"","browser":"safari","created_at":"2026-01-01T00:00:00Z","exported_at":"2026-01-01T00:00:00Z","bm_version":"0.0.5"}"#,
        )
        .unwrap();
        let err = import_profile(&storage, &safari, None, false).unwrap_err();
        assert!(err.to_string().contains("safari"), "got: {err}");
    }

    #[test]
    fn export_import_roundtrip_across_storages() {
        let (_tmp_a, storage_a) = setup();
        let tmp_b = tempfile::tempdir().unwrap();
        let storage_b = StorageManager::with_base_dir(tmp_b.path().join(".bm"));
        let browser = Browser::Brave;

        storage_a.create_profile("shared", Some("docs team"), browser).unwrap();
        storage_a
            .set_proxy("shared", browser, Some(ProxyConfig::parse("socks5://[::1]:1080", None).unwrap()))
            .unwrap();
        let bundle_path = export_profile(&storage_a, browser, "shared", None).unwrap();
        // Simulate a cross-machine move: read the bundle text, write it on "machine B".
        let text = fs::read_to_string(&bundle_path).unwrap();
        let on_b = tmp_b.path().join("shared.json");
        fs::write(&on_b, text).unwrap();

        let meta = import_profile(&storage_b, &on_b, None, false).unwrap();
        assert_eq!(meta.browser, Browser::Brave);
        assert_eq!(
            meta.proxy.unwrap().server_string(),
            "socks5://[::1]:1080"
        );
        assert!(storage_b.proxy_for(browser, "shared").is_some());
    }
}
