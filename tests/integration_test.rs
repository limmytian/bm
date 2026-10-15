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

    assert!(Browser::Chrome.default_data_dir().is_ok());
    assert!(Browser::Edge.default_data_dir().is_ok());
}
