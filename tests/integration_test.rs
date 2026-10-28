use bm::browser::Browser;
use bm::storage::StorageManager;
use tempfile::tempdir;

#[test]
fn test_profile_name_validation() {
    assert!(StorageManager::validate_name("work").is_ok());
    assert!(StorageManager::validate_name("personal-123_test").is_ok());
    assert!(StorageManager::validate_name("").is_err());
    assert!(StorageManager::validate_name(".").is_err());
    assert!(StorageManager::validate_name("..").is_err());
    assert!(StorageManager::validate_name("name with spaces").is_err());
    assert!(StorageManager::validate_name("invalid/slash").is_err());
}

#[test]
fn test_profile_lifecycle_in_isolated_storage() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
    let browser = Browser::Chrome;

    // 1. Create profiles
    let p1 = storage
        .create_profile("work", Some("Primary work account"), browser)
        .expect("Failed to create work profile");
    assert_eq!(p1.name, "work");
    assert_eq!(p1.description, "Primary work account");

    let p2 = storage
        .create_profile("personal", None, browser)
        .expect("Failed to create personal profile");
    assert_eq!(p2.name, "personal");
    assert_eq!(p2.description, "");

    // Creating duplicate should fail
    assert!(storage.create_profile("work", None, browser).is_err());

    // 2. List profiles
    let list = storage.list_profiles(browser).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].0.name, "personal");
    assert_eq!(list[1].0.name, "work");

    // 3. Describe / update notes
    let updated = storage
        .describe_profile("personal", "Updated personal note", browser)
        .unwrap();
    assert_eq!(updated.description, "Updated personal note");

    let reg = storage.load_registry().unwrap();
    let key = format!("{}:personal", browser);
    assert_eq!(
        reg.profiles.get(&key).unwrap().description,
        "Updated personal note"
    );

    // 4. Delete profile
    storage
        .delete_profile("personal", browser)
        .expect("Failed to delete profile");
    let list_after_delete = storage.list_profiles(browser).unwrap();
    assert_eq!(list_after_delete.len(), 1);
    assert_eq!(list_after_delete[0].0.name, "work");
}

#[test]
fn test_browser_properties() {
    assert_eq!(Browser::Chrome.display_name(), "Google Chrome");
    assert_eq!(Browser::Edge.display_name(), "Microsoft Edge");
    assert_eq!(Browser::Brave.display_name(), "Brave");
    assert_eq!(Browser::Chromium.display_name(), "Chromium");

    assert!(Browser::Chrome.default_data_dir().is_ok());
    assert!(Browser::Edge.default_data_dir().is_ok());
    assert!(Browser::Brave.default_data_dir().is_ok());
    assert!(Browser::Chromium.default_data_dir().is_ok());

    // Policy domains feed the macOS managed-policy path.
    assert_eq!(Browser::Chrome.policy_domain(), "com.google.Chrome");
    assert_eq!(Browser::Edge.policy_domain(), "com.microsoft.Edge");
    assert_eq!(Browser::Brave.policy_domain(), "com.brave.Browser");
    assert_eq!(Browser::Chromium.policy_domain(), "org.chromium.Chromium");
}

#[test]
fn test_brave_and_chromium_data_dirs() {
    // Brave uses the BraveSoftware layout on both platforms.
    let brave = Browser::Brave.default_data_dir().unwrap();
    assert!(brave.ends_with("BraveSoftware/Brave-Browser"));

    // Chromium: ~/Library/Application Support/Chromium (macOS)
    // or ~/.config/chromium (Linux).
    let chromium = Browser::Chromium.default_data_dir().unwrap();
    assert!(
        chromium.ends_with("Application Support/Chromium")
            || chromium.ends_with(".config/chromium"),
        "unexpected Chromium data dir: {}",
        chromium.display()
    );

    // Profile storage nests under the browser display key.
    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
    let meta = storage
        .create_profile("sandbox", None, Browser::Brave)
        .unwrap();
    assert_eq!(meta.browser, Browser::Brave);
    assert!(storage
        .profile_dir(Browser::Brave, "sandbox")
        .starts_with(tmp.path().join(".bm/profiles/brave")));

    // Not-installed browsers fail gracefully: on macOS the error names the
    // missing bundle path; elsewhere it is a platform error.
    if let Err(e) = Browser::Brave.app_path() {
        if cfg!(target_os = "macos") {
            let msg = e.to_string();
            assert!(msg.contains("Brave Browser.app"), "unhelpful error: {msg}");
        } else {
            assert!(e.to_string().contains("Platform unsupported"));
        }
    }
}

#[test]
fn test_proxy_persistence_and_backward_compatibility() {
    use bm::proxy::ProxyConfig;

    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
    let browser = Browser::Chrome;

    storage.create_profile("work", None, browser).unwrap();
    storage.create_profile("personal", None, browser).unwrap();

    // Set proxy on one profile; the other stays proxy-free.
    let cfg = ProxyConfig::parse("socks5://10.0.0.1:1080", Some("localhost;*.lan")).unwrap();
    let updated = storage
        .set_proxy("work", browser, Some(cfg.clone()))
        .unwrap();
    assert_eq!(updated.proxy, Some(cfg.clone()));
    assert_eq!(
        storage.set_proxy("personal", browser, None).unwrap().proxy,
        None
    );

    // Re-load from disk: proxy survives the roundtrip.
    let list = storage.list_profiles(browser).unwrap();
    let work = list.iter().find(|(m, _)| m.name == "work").unwrap();
    assert_eq!(
        work.0.proxy.as_ref().unwrap().server_string(),
        "socks5://10.0.0.1:1080"
    );
    assert_eq!(
        work.0.proxy.as_ref().unwrap().bypass_string().unwrap(),
        "localhost,*.lan"
    );

    // Clearing works too.
    let cleared = storage.set_proxy("work", browser, None).unwrap();
    assert_eq!(cleared.proxy, None);

    // A metadata.json written before proxy support (no proxy field) must load.
    let legacy = StorageManager::with_base_dir(tmp.path().join("legacy-bm"));
    let legacy_dir = legacy.profile_dir(browser, "old");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(
        legacy.metadata_file(),
        r#"{"profiles":{"chrome:old":{"name":"old","browser":"chrome","description":"","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}}"#,
    )
    .unwrap();
    let list = legacy.list_profiles(browser).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0.proxy, None);

    // A profile that exists only as a folder (no metadata) gets its record
    // synthesized on first set_proxy, mirroring describe_profile behavior.
    let tmp2 = tempdir().unwrap();
    let storage2 = StorageManager::with_base_dir(tmp2.path().join(".bm2"));
    std::fs::create_dir_all(storage2.profile_dir(browser, "orphandir")).unwrap();
    let orphan = storage2.set_proxy("orphandir", browser, Some(cfg)).unwrap();
    assert_eq!(
        orphan.proxy.as_ref().unwrap().server_string(),
        "socks5://10.0.0.1:1080"
    );
}

#[test]
fn test_proxy_lookup_helper() {
    use bm::proxy::ProxyConfig;

    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
    let browser = Browser::Chrome;

    // Unknown profile: no proxy.
    assert_eq!(storage.proxy_for(browser, "work"), None);

    // A metadata-less folder also resolves to no proxy (launcher fallback).
    std::fs::create_dir_all(storage.profile_dir(browser, "orphandir")).unwrap();
    assert_eq!(storage.proxy_for(browser, "orphandir"), None);

    let cfg = ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap();
    storage.create_profile("work", None, browser).unwrap();
    storage
        .set_proxy("work", browser, Some(cfg.clone()))
        .unwrap();
    assert_eq!(storage.proxy_for(browser, "work"), Some(cfg));

    // Clearing the proxy is reflected in the lookup.
    storage.set_proxy("work", browser, None).unwrap();
    assert_eq!(storage.proxy_for(browser, "work"), None);
}

#[test]
fn test_template_lifecycle_and_seeding_new_profile() {
    use bm::template::{apply_template, delete_template, list_templates, save_template};
    use std::fs;

    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));
    let browser = Browser::Chrome;

    // Create source profile
    storage
        .create_profile("base-dev", Some("Base dev profile"), browser)
        .unwrap();
    let src_dir = storage.profile_dir(browser, "base-dev");
    let default_dir = src_dir.join("Default");
    fs::create_dir_all(&default_dir).unwrap();

    // Reusable bookmarks & extension
    fs::write(default_dir.join("Bookmarks"), r#"{"roots":{}}"#).unwrap();
    let ext_dir = default_dir
        .join("Extensions")
        .join("react-dev-tools")
        .join("1.0.0");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join("manifest.json"),
        r#"{"name":"react-dev-tools"}"#,
    )
    .unwrap();

    // Sensitive cookies & logins to be stripped
    fs::write(default_dir.join("Cookies"), b"cookie-db").unwrap();
    fs::write(default_dir.join("Login Data"), b"passwords-db").unwrap();
    fs::create_dir_all(default_dir.join("Sessions")).unwrap();
    fs::write(
        default_dir.join("Sessions").join("Session_123"),
        b"session-data",
    )
    .unwrap();

    // 1. Save template
    let tpl = save_template(
        &storage,
        browser,
        "dev-box",
        "base-dev",
        Some("Dev box template with React dev tools"),
        false,
    )
    .unwrap();
    assert_eq!(tpl.name, "dev-box");
    assert!(tpl.components.contains(&"Bookmarks".to_string()));
    assert!(tpl.components.contains(&"Extensions".to_string()));

    // 2. List templates
    let list = list_templates(&storage, Some(browser)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "dev-box");

    // 3. Seed new profile from template
    storage
        .create_profile("sandbox-1", Some("New seeded sandbox"), browser)
        .unwrap();
    let target_dir = storage.profile_dir(browser, "sandbox-1");
    let applied = apply_template(&storage, "dev-box", &target_dir).unwrap();
    assert_eq!(applied.name, "dev-box");

    // Verify seeded profile has Bookmarks and Extensions, but ZERO cookies/sessions/logins!
    assert!(target_dir.join("Default").join("Bookmarks").exists());
    assert!(target_dir
        .join("Default")
        .join("Extensions")
        .join("react-dev-tools")
        .join("1.0.0")
        .join("manifest.json")
        .exists());
    assert!(!target_dir.join("Default").join("Cookies").exists());
    assert!(!target_dir.join("Default").join("Login Data").exists());
    assert!(!target_dir.join("Default").join("Sessions").exists());

    // 4. Delete template
    delete_template(&storage, "dev-box").unwrap();
    assert!(list_templates(&storage, Some(browser)).unwrap().is_empty());
}

#[test]
fn test_group_lifecycle_and_management() {
    use bm::group::{
        add_member, create_group, delete_group, get_group, list_groups, remove_member, status_group,
    };
    use bm::proxy::ProxyConfig;

    let tmp = tempdir().unwrap();
    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"));

    // 1. Create profiles
    storage
        .create_profile("chrome-work", Some("Work on Chrome"), Browser::Chrome)
        .unwrap();
    storage
        .create_profile("edge-ops", Some("Ops on Edge"), Browser::Edge)
        .unwrap();

    // 2. Create group
    let group = create_group(&storage, "work-suite", Some("Company work suite")).unwrap();
    assert_eq!(group.name, "work-suite");
    assert_eq!(group.members.len(), 0);

    // 3. Add members across browsers
    let updated = add_member(
        &storage,
        "work-suite",
        Browser::Chrome,
        "chrome-work",
        vec!["https://jira.example.com".into()],
        None,
    )
    .unwrap();
    assert_eq!(updated.members.len(), 1);

    let proxy = ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap();
    let updated = add_member(
        &storage,
        "work-suite",
        Browser::Edge,
        "edge-ops",
        vec!["https://grafana.example.com".into()],
        Some(proxy),
    )
    .unwrap();
    assert_eq!(updated.members.len(), 2);

    // 4. Query group and status
    let loaded = get_group(&storage, "work-suite").unwrap();
    assert_eq!(loaded.members.len(), 2);
    assert_eq!(loaded.members[0].profile, "chrome-work");
    assert_eq!(loaded.members[1].profile, "edge-ops");

    let status = status_group(&storage, "work-suite").unwrap();
    assert_eq!(status.member_count, 2);
    assert_eq!(status.running_count, 0);

    let all = list_groups(&storage).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "work-suite");

    // 5. Remove member
    let after_remove =
        remove_member(&storage, "work-suite", Browser::Chrome, "chrome-work").unwrap();
    assert_eq!(after_remove.members.len(), 1);
    assert_eq!(after_remove.members[0].profile, "edge-ops");

    // 6. Delete group
    delete_group(&storage, "work-suite").unwrap();
    assert!(list_groups(&storage).unwrap().is_empty());
}

#[test]
fn test_lease_locking_and_switch_guard() {
    use bm::lease::{claim_lease, heartbeat_lease, list_leases, release_lease, ClaimOptions};

    let tmp = tempdir().unwrap();
    let fake_default = tmp.path().join("fake_default");
    std::fs::create_dir_all(&fake_default).unwrap();

    let storage = StorageManager::with_base_dir(tmp.path().join(".bm"))
        .with_default_dir_override(fake_default);
    let browser = Browser::Chrome;

    storage.create_profile("locked-p1", None, browser).unwrap();
    storage.create_profile("target-p2", None, browser).unwrap();

    // 1. Claim lease on locked-p1
    let lease = claim_lease(
        &storage,
        browser,
        "locked-p1",
        ClaimOptions {
            holder: "agent-smith",
            ttl_secs: Some(120),
            reason: Some("Processing CDP queue"),
            pid: Some(std::process::id()),
            force: false,
        },
    )
    .unwrap();
    assert_eq!(lease.holder, "agent-smith");

    // 2. Listing leases returns locked-p1
    let active = list_leases(&storage, Some(browser), None).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].profile, "locked-p1");

    // 3. Destructive actions without force are blocked
    let switch_err = storage.switch_profile("locked-p1", browser, false);
    assert!(switch_err.is_err());
    let del_err = storage.delete_profile("locked-p1", browser);
    assert!(del_err.is_err());
    let rename_err = storage.rename_profile("locked-p1", "new-p1", browser);
    assert!(rename_err.is_err());

    // 4. Force switch allows bypass
    assert!(storage.switch_profile("locked-p1", browser, true).is_ok());

    // 5. Heartbeat extends expiry
    let extended = heartbeat_lease(&storage, browser, "locked-p1", Some(300)).unwrap();
    assert!(extended.expires_at >= lease.expires_at);

    // 6. Release lease unlocks the profile
    release_lease(&storage, browser, "locked-p1", Some("agent-smith"), false).unwrap();
    assert!(list_leases(&storage, Some(browser), None)
        .unwrap()
        .is_empty());

    // 7. Switch to target-p2 and delete locked-p1 succeeds after release
    storage.switch_profile("target-p2", browser, false).unwrap();
    assert!(storage.delete_profile("locked-p1", browser).is_ok());
}
