//! Local profile snapshots (`bm backup` / `bm restore`).
//!
//! Archives live under `~/.bm/backups` as
//! `<browser>-<name>-<YYYYmmddHHMMSS>.tar.zst`. A `manifest.json` entry is
//! embedded at backup time so a restore can register the profile's
//! description and egress proxy without asking again. Regenerable caches are
//! excluded unless `--full` (see src/cache.rs). This is intentionally a
//! local-machine feature — no cross-machine sync.

use std::collections::hash_map::Entry;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::{ProfileMetadata, StorageManager};

const MANIFEST_ENTRY: &str = "manifest.json";

/// Embedded per-archive metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupManifest {
    name: String,
    browser: String,
    description: String,
    proxy: Option<crate::proxy::ProxyConfig>,
    created_at: DateTime<Utc>,
    bm_version: String,
}

/// Result of one backup run.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    pub path: PathBuf,
    pub archive_bytes: u64,
    /// Full size of the source profile (cache included) for the ratio print.
    pub profile_bytes: u64,
    /// Number of older archives removed by `--keep`.
    pub pruned: usize,
}

fn backups_dir(storage: &StorageManager) -> PathBuf {
    storage.base_dir().join("backups")
}

/// Create a snapshot of `name`. Refuses while the browser is running.
pub fn create_backup(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    full: bool,
    keep: Option<usize>,
) -> Result<BackupOutcome> {
    if browser.is_running() {
        return Err(BmError::BrowserRunning {
            browser: browser.display_name().to_string(),
        });
    }
    create_backup_unchecked(storage, browser, name, full, keep)
}

/// Backup without the running-browser guard; separate so tests can run
/// against `is_running`'s process-table dependency.
pub fn create_backup_unchecked(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    full: bool,
    keep: Option<usize>,
) -> Result<BackupOutcome> {
    let profile_dir = storage.profile_dir(browser, name);
    if !profile_dir.exists() {
        return Err(BmError::ProfileNotFound {
            name: name.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    let dir = backups_dir(storage);
    fs::create_dir_all(&dir)?;
    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    let path = dir.join(format!("{browser}-{name}-{timestamp}.tar.zst"));

    let profile_bytes = crate::size::measure(&profile_dir)?.total_bytes;
    let archive_bytes = write_archive(storage, browser, name, &profile_dir, &path, full)?;
    let pruned = match keep {
        Some(n) => prune_old_backups(&dir, browser, name, n)?,
        None => 0,
    };

    Ok(BackupOutcome {
        path,
        archive_bytes,
        profile_bytes,
        pruned,
    })
}

fn write_archive(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    profile_dir: &Path,
    out_path: &Path,
    full: bool,
) -> Result<u64> {
    let meta = storage.load_registry()?;
    let registered = meta
        .profiles
        .get(&format!("{browser}:{name}"));
    let manifest = BackupManifest {
        name: name.to_string(),
        browser: browser.to_string(),
        description: registered.map(|m| m.description.clone()).unwrap_or_default(),
        proxy: registered.and_then(|m| m.proxy.clone()),
        created_at: Utc::now(),
        bm_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;

    let file = fs::File::create(out_path)?;
    let encoder = zstd::Encoder::new(file, 3)?;
    let mut tar = tar::Builder::new(encoder);

    // Manifest first so a streaming restore can read it immediately.
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, MANIFEST_ENTRY, manifest_json.as_slice())?;

    append_tree(&mut tar, profile_dir, Path::new(""), !full)?;

    let encoder = tar.into_inner()?;
    encoder.finish()?;
    Ok(fs::metadata(out_path)?.len())
}

/// Recursively append files under `dir`, skipping regenerable entries when
/// `skip_caches`. Only regular files and symlinks are archived.
fn append_tree<W: Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    rel: &Path,
    skip_caches: bool,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let child_rel = rel.join(entry.file_name());
        if skip_caches && crate::cache::is_regenerable(&child_rel) {
            continue;
        }
        let child_path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            // Recreate symlinks as symlinks, target as-is (possibly broken —
            // runtime artifacts like SingletonSocket).
            let target = fs::read_link(&child_path)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            tar.append_link(&mut header, &child_rel, target)?;
        } else if file_type.is_dir() {
            append_tree(tar, &child_path, &child_rel, skip_caches)?;
        } else if file_type.is_file() {
            append_file_entry(tar, &child_rel, &child_path)?;
        }
    }
    Ok(())
}

fn append_file_entry<W: Write>(
    tar: &mut tar::Builder<W>,
    archive_name: &Path,
    content_path: &Path,
) -> Result<()> {
    let stat = fs::metadata(content_path)?;
    let mut file = fs::File::open(content_path)?;

    let mut header = tar::Header::new_gnu();
    header.set_size(stat.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        header.set_mode(stat.permissions().mode() & 0o7777);
    }
    #[cfg(not(unix))]
    header.set_mode(0o644);
    if let Ok(modified) = stat.modified() {
        if let Ok(secs) = modified.duration_since(std::time::UNIX_EPOCH) {
            header.set_mtime(secs.as_secs());
        }
    }
    header.set_cksum();
    tar.append_data(&mut header, archive_name, &mut file)?;
    Ok(())
}

/// Remove the oldest archives of one profile beyond `keep`; returns how many
/// were deleted. Timestamps in the filename sort lexicographically.
pub fn prune_old_backups(dir: &Path, browser: Browser, name: &str, keep: usize) -> Result<usize> {
    let prefix = format!("{browser}-{name}-");
    let mut archives: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && n.ends_with(".tar.zst"))
                .unwrap_or(false)
        })
        .collect();
    archives.sort();
    archives.reverse(); // newest first

    let mut removed = 0;
    for old in archives.into_iter().skip(keep) {
        fs::remove_file(&old)?;
        removed += 1;
    }
    Ok(removed)
}

/// Restore an archive into `~/.bm/profiles/<manifest-browser>/<name>`
/// (default: the archived name; `rename_to` overrides). Refuses to overwrite
/// an existing profile unless `force`. The browser comes from the embedded
/// manifest, not the `--browser` flag. Refuses while that browser is
/// running.
pub fn restore_archive(
    storage: &StorageManager,
    archive: &Path,
    rename_to: Option<&str>,
    force: bool,
) -> Result<ProfileMetadata> {
    let manifest = peek_manifest(archive)?;
    let browser = Browser::deserialize_manifest_browser(&manifest.browser)?;
    if browser.is_running() {
        return Err(BmError::BrowserRunning {
            browser: browser.display_name().to_string(),
        });
    }
    restore_archive_unchecked(storage, archive, rename_to, force)
}

/// Restore without the running-browser guard; separate so tests can run
/// against `is_running`'s process-table dependency.
pub fn restore_archive_unchecked(
    storage: &StorageManager,
    archive: &Path,
    rename_to: Option<&str>,
    force: bool,
) -> Result<ProfileMetadata> {
    let manifest = peek_manifest(archive)?;
    let browser = Browser::deserialize_manifest_browser(&manifest.browser)?;
    let name = match rename_to {
        Some(n) => {
            StorageManager::validate_name(n)?;
            n.to_string()
        }
        None => {
            StorageManager::validate_name(&manifest.name)?;
            manifest.name.clone()
        }
    };

    let dest = storage.profile_dir(browser, &name);
    if dest.exists() {
        if !force {
            return Err(BmError::ProfileAlreadyExists {
                name: name.clone(),
                browser: browser.display_name().to_string(),
            });
        }
        fs::remove_dir_all(&dest)?;
    }
    storage.ensure_dirs(browser)?;
    fs::create_dir_all(&dest)?;

    extract_archive(archive, &dest)?;

    // Register the profile with description and proxy from the manifest.
    let mut reg = storage.load_registry()?;
    let now = Utc::now();
    let meta = ProfileMetadata {
        name: name.clone(),
        browser,
        description: manifest.description,
        created_at: now,
        updated_at: now,
        proxy: manifest.proxy,
    };
    match reg.profiles.entry(format!("{browser}:{name}")) {
        Entry::Vacant(slot) => {
            slot.insert(meta.clone());
        }
        Entry::Occupied(mut slot) => {
            slot.get_mut().description = meta.description.clone();
            slot.get_mut().proxy = meta.proxy.clone();
            slot.get_mut().updated_at = now;
        }
    }
    storage.save_registry(&reg)?;

    Ok(meta)
}

/// Parse the embedded manifest from an archive file.
fn peek_manifest(archive: &Path) -> Result<BackupManifest> {
    let file = fs::File::open(archive)
        .map_err(|e| BmError::Other(format!("cannot open archive {}: {e}", archive.display())))?;
    let decoder = zstd::Decoder::new(file)?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path == Path::new(MANIFEST_ENTRY) {
            let mut buf = String::new();
            entry.read_to_string(&mut buf)?;
            return Ok(serde_json::from_str(&buf)?);
        }
    }
    Err(BmError::Other(format!(
        "no {MANIFEST_ENTRY} in {} — not a bm backup",
        archive.display()
    )))
}

/// Extract all entries below `dest`, rejecting path traversal (absolute
/// paths, `..`, prefixes) so a hostile archive cannot escape the profile dir.
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive)
        .map_err(|e| BmError::Other(format!("cannot open archive {}: {e}", archive.display())))?;
    let decoder = zstd::Decoder::new(file)?;
    let mut tar = tar::Archive::new(decoder);

    for entry in tar.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.to_path_buf();
        let rel = safe_rel_path(&raw).ok_or_else(|| {
            BmError::Other(format!(
                "archive entry '{}' escapes the profile dir — refusing",
                raw.display()
            ))
        })?;
        let target = dest.join(rel);

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(&target)?;
        }
    }
    Ok(())
}

/// Normalize an archive path into safe relative components.
fn safe_rel_path(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return None, // Root, Prefix, ParentDir → reject
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

impl Browser {
    /// Map a manifest browser string back to the enum.
    fn deserialize_manifest_browser(raw: &str) -> Result<Browser> {
        match raw {
            "chrome" => Ok(Browser::Chrome),
            "edge" => Ok(Browser::Edge),
            "brave" => Ok(Browser::Brave),
            "chromium" => Ok(Browser::Chromium),
            other => Err(BmError::Other(format!(
                "archive manifest names unsupported browser '{other}'"
            ))),
        }
    }
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

    fn seed(storage: &StorageManager, browser: Browser, name: &str) {
        let cfg = crate::proxy::ProxyConfig::parse("socks5://10.0.0.1:1080", None).unwrap();
        storage
            .create_profile(name, Some("snapshot source"), browser)
            .unwrap();
        storage.set_proxy(name, browser, Some(cfg)).unwrap();
        let root = storage.profile_dir(browser, name);
        fs::create_dir_all(root.join("Default")).unwrap();
        fs::write(root.join("Default/Cookies"), "cookie-db").unwrap();
        fs::create_dir_all(root.join("Cache")).unwrap();
        fs::write(root.join("Cache/data_0"), "cache").unwrap();
    }

    #[test]
    fn backup_restore_roundtrip_preserves_core_and_metadata() {
        let (tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");

        let outcome =
            create_backup_unchecked(&storage, browser, "work", false, None).unwrap();
        assert!(outcome.path.exists());
        assert!(outcome.archive_bytes > 0);
        assert!(outcome.profile_bytes > 0);

        // Restore under a new name (renamed restore).
        let meta = restore_archive_unchecked(
            &storage,
            &outcome.path,
            Some("work-restored"),
            false,
        )
        .unwrap();
        assert_eq!(meta.name, "work-restored");
        assert_eq!(meta.description, "snapshot source");
        assert_eq!(
            meta.proxy.as_ref().unwrap().server_string(),
            "socks5://10.0.0.1:1080"
        );

        let dst = storage.profile_dir(browser, "work-restored");
        assert_eq!(fs::read_to_string(dst.join("Default/Cookies")).unwrap(), "cookie-db");
        // Cache was excluded from the backup.
        assert!(!dst.join("Cache").exists());

        let reg = storage.load_registry().unwrap();
        assert!(reg.profiles.contains_key("chrome:work-restored"));
        let _ = tmp;
    }

    #[test]
    fn full_backup_includes_caches() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");

        let outcome = create_backup_unchecked(&storage, browser, "work", true, None).unwrap();
        let meta = restore_archive_unchecked(&storage, &outcome.path, Some("full"), false).unwrap();
        let _ = meta;
        assert!(storage
            .profile_dir(browser, "full/Cache/data_0")
            .exists());
    }

    #[test]
    fn restore_refuses_existing_profile_unless_forced() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");
        let outcome = create_backup_unchecked(&storage, browser, "work", false, None).unwrap();

        assert!(restore_archive_unchecked(&storage, &outcome.path, None, false).is_err());
        restore_archive_unchecked(&storage, &outcome.path, None, true).unwrap();
        // Existing dir was replaced by the archive content.
        assert_eq!(
            fs::read_to_string(storage.profile_dir(browser, "work/Default/Cookies")).unwrap(),
            "cookie-db"
        );
    }

    #[test]
    fn keep_pruning_removes_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backups");
        fs::create_dir_all(&dir).unwrap();
        for ts in ["20260101000000", "20260102000000", "20260103000000"] {
            fs::write(dir.join(format!("chrome-work-{ts}.tar.zst")), "x").unwrap();
        }
        // Other profiles' archives are untouched.
        fs::write(dir.join("chrome-other-20260104000000.tar.zst"), "x").unwrap();

        let removed = prune_old_backups(&dir, Browser::Chrome, "work", 2).unwrap();
        assert_eq!(removed, 1);
        assert!(dir.join("chrome-work-20260102000000.tar.zst").exists());
        assert!(dir.join("chrome-work-20260103000000.tar.zst").exists());
        assert!(!dir.join("chrome-work-20260101000000.tar.zst").exists());
        assert!(dir.join("chrome-other-20260104000000.tar.zst").exists());
    }

    #[test]
    fn traversal_entries_are_rejected() {
        let (_tmp, storage) = setup();

        // Craft a hostile archive with a ../ entry. tar-rs refuses to WRITE
        // such paths, so build the raw 512-byte tar blocks by hand and wrap
        // them in a zstd frame the same way real archives are stored.
        let evil = tempfile::tempdir().unwrap();
        let archive_path = evil.path().join("evil.tar.zst");
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(&raw_tar_header("../escaped.txt", 5));
        raw.extend_from_slice(b"pwned\0\0\0");
        raw.extend_from_slice(&[0u8; 1024]); // two zero blocks end the tar
        fs::write(
            &archive_path,
            zstd::stream::encode_all(&raw[..], 3).unwrap(),
        )
        .unwrap();

        let result = restore_archive_unchecked(&storage, &archive_path, None, false);
        assert!(result.is_err(), "traversal must be refused");
        assert!(
            !evil.path().parent().unwrap().join("escaped.txt").exists(),
            "nothing may be written outside the target dir"
        );
    }

    /// A minimal valid ustar header for `name` with a regular-file payload of
    /// `size` bytes (checksum computed per the tar spec).
    fn raw_tar_header(name: &str, size: usize) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0"); // mode
        h[108..116].copy_from_slice(b"0000000\0"); // uid
        h[116..124].copy_from_slice(b"0000000\0"); // gid
        h[124..136].copy_from_slice(format!("{:011o}\0", size).as_bytes()); // size
        h[136..148].copy_from_slice(b"00000000000\0"); // mtime
        h[148..156].copy_from_slice(b"        "); // checksum placeholder (spaces)
        h[156] = b'0'; // typeflag: regular file
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        let chk = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(chk.as_bytes());
        h
    }

    #[test]
    fn symlinks_survive_the_roundtrip() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");
        let root = storage.profile_dir(browser, "work");
        symlink("Cookies", root.join("Default/link")).unwrap();

        let outcome = create_backup_unchecked(&storage, browser, "work", true, None).unwrap();
        restore_archive_unchecked(&storage, &outcome.path, Some("linked"), false).unwrap();
        let link = storage.profile_dir(browser, "linked/Default/link");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("Cookies"));
    }
}
