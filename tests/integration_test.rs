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
        chromium.ends_with("Application Support/Chromium") || chromium.ends_with("config/chromium"),
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

    // Not-installed browsers fail gracefully with the bundle path named.
    if let Err(e) = Browser::Brave.app_path() {
        let msg = e.to_string();
        assert!(msg.contains("Brave Browser.app"), "unhelpful error: {msg}");
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
    let updated = storage.set_proxy("work", browser, Some(cfg.clone())).unwrap();
    assert_eq!(updated.proxy, Some(cfg.clone()));
    assert_eq!(storage.set_proxy("personal", browser, None).unwrap().proxy, None);

    // Re-load from disk: proxy survives the roundtrip.
    let list = storage.list_profiles(browser).unwrap();
    let work = list.iter().find(|(m, _)| m.name == "work").unwrap();
    assert_eq!(work.0.proxy.as_ref().unwrap().server_string(), "socks5://10.0.0.1:1080");
    assert_eq!(work.0.proxy.as_ref().unwrap().bypass_string().unwrap(), "localhost,*.lan");

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
    assert_eq!(orphan.proxy.as_ref().unwrap().server_string(), "socks5://10.0.0.1:1080");
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
    storage.set_proxy("work", browser, Some(cfg.clone())).unwrap();
    assert_eq!(storage.proxy_for(browser, "work"), Some(cfg));

    // Clearing the proxy is reflected in the lookup.
    storage.set_proxy("work", browser, None).unwrap();
    assert_eq!(storage.proxy_for(browser, "work"), None);
}
