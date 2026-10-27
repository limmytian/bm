//! Stable JSON output for scripted runs (`bm --json <command>`).
//!
//! Every function here returns a [`serde_json::Value`] whose shape is
//! documented in `docs/json.md` (schema version 1) and pinned by golden
//! tests in this module — external tooling may rely on these shapes
//! verbatim. The human renderers in main.rs stay untouched; MCP tools (v1)
//! return these same values.

use serde_json::{json, Value};

use crate::browser::Browser;
use crate::error::Result;
use crate::storage::StorageManager;

/// `bm --json list`
pub fn list_json(storage: &StorageManager, browser: Browser) -> Result<Value> {
    let last_switched_at = storage.last_switched_at(browser);
    let profiles = storage
        .list_profiles(browser)?
        .into_iter()
        .map(|(meta, is_active)| {
            json!({
                "name": meta.name,
                "description": meta.description,
                "active": is_active,
                "proxy": meta.proxy.map(|p| p.to_string()),
                "last_switched_at": (is_active && last_switched_at.is_some())
                    .then(|| last_switched_at.unwrap().to_rfc3339()),
                "created_at": meta.created_at.to_rfc3339(),
                "updated_at": meta.updated_at.to_rfc3339(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "browser": browser.to_string(), "profiles": profiles }))
}

/// `bm --json current`
pub fn current_json(storage: &StorageManager, browser: Browser) -> Result<Value> {
    let active = storage.get_current_active(browser)?;
    let proxy = active
        .as_ref()
        .and_then(|name| storage.proxy_for(browser, name))
        .map(|p| p.to_string());
    let managed = active
        .as_ref()
        .map(|a| !a.starts_with("external ("))
        .unwrap_or(false);
    Ok(json!({
        "browser": browser.to_string(),
        // The active profile name, "external (...)" for foreign activations,
        // or null when the browser runs on unmanaged data.
        "active": active,
        // False for external/unmanaged activations: no bm profile is loaded.
        "managed": managed,
        "proxy": proxy,
    }))
}

/// `bm --json doctor` (with `fix`, `applied` counts the repairs applied and
/// `issues` carries the findings still open afterwards).
pub fn doctor_json(storage: &StorageManager, browser: Browser, fix: bool) -> Result<Value> {
    let found = crate::doctor::run_checks(storage, browser)?;
    let applied = if fix {
        crate::doctor::apply_fixes(storage, &found)?
    } else {
        0
    };
    let findings: Vec<Value> = found
        .iter()
        .map(|issue| {
            json!({
                "check": issue.check,
                "detail": issue.detail,
                "auto_fixable": issue.repair.is_some(),
            })
        })
        .collect();
    // What remains after the safe repairs: a fresh check run with --fix,
    // the original findings without.
    let remaining_issues = if fix {
        crate::doctor::run_checks(storage, browser)?
    } else {
        found
    };
    Ok(json!({
        "browser": browser.to_string(),
        "issues": findings,
        "issue_count": findings.len(),
        "auto_fixable": findings.iter().filter(|i| i["auto_fixable"] == json!(true)).count(),
        "applied": applied,
        "remaining": remaining_issues.len(),
    }))
}

/// `bm --json proxy show [name]` — one shape for both forms; `name` filters
/// to a single entry.
pub fn proxy_json(storage: &StorageManager, browser: Browser, name: Option<&str>) -> Result<Value> {
    let profiles = storage
        .list_profiles(browser)?
        .into_iter()
        .filter(|(meta, _)| name.is_none_or(|n| meta.name == n))
        .map(|(meta, is_active)| {
            json!({
                "name": meta.name,
                "active": is_active,
                "proxy": meta.proxy.as_ref().map(|p| p.to_string()),
                "bypass": meta.proxy.as_ref().and_then(|p| p.bypass_string()),
            })
        })
        .collect::<Vec<_>>();
    if let Some(n) = name {
        if profiles.is_empty() {
            return Err(crate::error::BmError::ProfileNotFound {
                name: n.to_string(),
                browser: browser.display_name().to_string(),
            });
        }
    }
    Ok(json!({ "browser": browser.to_string(), "profiles": profiles }))
}

/// `bm --json status` — the same report the human table renders.
pub fn status_json(storage: &StorageManager, browser: Browser) -> Result<Value> {
    serde_json::to_value(crate::status::collect_status(storage, browser)?)
        .map_err(|e| crate::error::BmError::Other(format!("cannot serialize status: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ProxyConfig;

    fn setup() -> (tempfile::TempDir, StorageManager) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        std::fs::create_dir_all(&base).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        (tmp, StorageManager::with_base_dir(base))
    }

    fn seed_work(storage: &StorageManager, browser: Browser) {
        storage
            .create_profile("work", Some("day job"), browser)
            .unwrap();
        storage
            .set_proxy(
                "work",
                browser,
                Some(
                    ProxyConfig::parse("socks5://10.0.0.1:1080", Some("localhost;*.lan")).unwrap(),
                ),
            )
            .unwrap();
        storage.create_profile("personal", None, browser).unwrap();
    }

    /// Golden shape for `bm --json list`.
    #[test]
    fn list_json_shape() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed_work(&storage, browser);
        // 'work' active via the overridden symlink path is not needed for
        // the shape; both entries start inactive here.
        let v = list_json(&storage, browser).unwrap();

        assert_eq!(v["browser"], "chrome");
        let profiles = v["profiles"].as_array().unwrap();
        assert_eq!(profiles.len(), 2);
        let work = profiles.iter().find(|p| p["name"] == "work").unwrap();
        // Exact key set — new keys require a docs/json.md version bump.
        let keys: Vec<&str> = work
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec![
                "active",
                "created_at",
                "description",
                "last_switched_at",
                "name",
                "proxy",
                "updated_at"
            ]
        );
        assert_eq!(work["description"], "day job");
        assert_eq!(work["proxy"], "socks5://10.0.0.1:1080");
        assert_eq!(work["active"], false);
        assert_eq!(work["last_switched_at"], serde_json::Value::Null);
    }

    /// Golden shape for `bm --json current` (active + external + none).
    #[test]
    fn current_json_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        std::fs::create_dir_all(&base).unwrap();
        let default_link = tmp.path().join("default-link");
        let storage =
            StorageManager::with_base_dir(base).with_default_dir_override(default_link.clone());
        let browser = Browser::Chrome;

        // None: unmanaged.
        let v = current_json(&storage, browser).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"browser": "chrome", "active": null, "managed": false, "proxy": null})
        );

        // Managed active profile.
        storage.create_profile("work", None, browser).unwrap();
        std::os::unix::fs::symlink(storage.profile_dir(browser, "work"), &default_link).unwrap();
        storage
            .set_proxy(
                "work",
                browser,
                Some(ProxyConfig::parse("http://p:8080", None).unwrap()),
            )
            .unwrap();
        let v = current_json(&storage, browser).unwrap();
        assert_eq!(v["active"], "work");
        assert_eq!(v["managed"], true);
        assert_eq!(v["proxy"], "http://p:8080");

        // External activation.
        std::fs::remove_file(&default_link).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &default_link).unwrap();
        let v = current_json(&storage, browser).unwrap();
        assert!(v["active"].as_str().unwrap().starts_with("external ("));
        assert_eq!(v["managed"], false);
    }

    /// Golden shape for `bm --json doctor`.
    #[test]
    fn doctor_json_shape() {
        let (tmp, storage) = setup();
        let browser = Browser::Chrome;
        let link = tmp.path().join("default-link");
        let storage = storage.with_default_dir_override(link.clone());
        std::os::unix::fs::symlink(tmp.path().join("missing"), &link).unwrap();

        let v = doctor_json(&storage, browser, false).unwrap();
        assert_eq!(v["browser"], "chrome");
        assert_eq!(v["issue_count"], 1);
        assert_eq!(v["applied"], 0);
        assert_eq!(v["issues"][0]["check"], "default-symlink");
        let keys: Vec<&str> = v["issues"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["auto_fixable", "check", "detail"]);

        // With fix: applied=1 and a fresh check finds nothing left.
        let v = doctor_json(&storage, browser, true).unwrap();
        assert_eq!(v["applied"], 1);
        assert_eq!(v["remaining"], 0);
    }

    /// Golden shape for `bm --json proxy show [name]`.
    #[test]
    fn proxy_json_shape() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed_work(&storage, browser);

        let v = proxy_json(&storage, browser, None).unwrap();
        assert_eq!(v["browser"], "chrome");
        assert_eq!(v["profiles"].as_array().unwrap().len(), 2);
        let work = v["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "work")
            .unwrap();
        let keys: Vec<&str> = work
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["active", "bypass", "name", "proxy"]);
        assert_eq!(work["proxy"], "socks5://10.0.0.1:1080");
        assert_eq!(work["bypass"], "localhost,*.lan");

        let v = proxy_json(&storage, browser, Some("work")).unwrap();
        assert_eq!(v["profiles"].as_array().unwrap().len(), 1);
        assert!(proxy_json(&storage, browser, Some("ghost")).is_err());
    }

    /// Golden shape for `bm --json status` (same StatusReport as the table).
    #[test]
    fn status_json_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(".bm");
        std::fs::create_dir_all(&base).unwrap();
        let default_link = tmp.path().join("default-link");
        let storage =
            StorageManager::with_base_dir(base).with_default_dir_override(default_link.clone());
        let browser = Browser::Chrome;
        seed_work(&storage, browser);

        // No active profile: the optional key is omitted entirely.
        let v = status_json(&storage, browser).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["browser", "doctor", "profiles"]);
        assert_eq!(v["browser"], "chrome");
        // A fully-populated entry carries every key; empty description and a
        // missing proxy are omitted (skip_serializing_if).
        let work = v["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "work")
            .unwrap();
        let keys: Vec<&str> = work
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["active", "description", "name", "proxy", "running"]
        );
        let bare = v["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "personal")
            .unwrap();
        let keys: Vec<&str> = bare
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["active", "name", "running"]);
        assert!(v["doctor"].is_object());

        // With an active profile the key appears.
        std::os::unix::fs::symlink(storage.profile_dir(browser, "work"), &default_link).unwrap();
        let v = status_json(&storage, browser).unwrap();
        assert_eq!(v["active_profile"], "work");
    }

    /// `bm size` keeps its shipped bare-array shape under the global flag.
    #[test]
    fn size_json_stays_an_array() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed_work(&storage, browser);
        let entry = serde_json::json!({
            "name": "work",
            "total_bytes": 0,
            "cache_bytes": 0,
            "core_bytes": 0,
        });
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["cache_bytes", "core_bytes", "name", "total_bytes"]
        );
    }
}
