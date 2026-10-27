//! Per-profile disk accounting and cache purging, shared by `bm size` and
//! `bm clean`.
//!
//! A profile dir is walked once; every entry is classified through
//! `crate::cache::is_regenerable` so both commands agree on what counts as
//! reclaimable cache versus valuable core data. Reads are racy while the
//! browser writes — sizes are best-effort snapshots.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// Byte accounting for one profile dir.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileSize {
    pub total_bytes: u64,
    /// Regenerable state (see src/cache.rs) — reclaimable via `bm clean`.
    pub cache_bytes: u64,
    /// Everything valuable: sessions, cookies, history, logins, extensions.
    pub core_bytes: u64,
}

/// Measure a profile dir in a single walk, splitting bytes into regenerable
/// cache and core data. Symlinks are never followed (counted as 0 bytes).
pub fn measure(profile_dir: &Path) -> Result<ProfileSize> {
    let mut size = ProfileSize::default();
    walk(profile_dir, Path::new(""), &mut size)?;
    size.total_bytes = size.cache_bytes + size.core_bytes;
    Ok(size)
}

/// Regenerable entries at the top boundary of the tree: `(relative path,
/// bytes beneath it)`. Nothing is mutated — `bm clean --dry-run` lists from
/// this scan.
pub fn scan_regenerable(profile_dir: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    scan_inner(profile_dir, Path::new(""), &mut found)?;
    Ok(found)
}

/// Delete regenerable entries (and everything beneath them) inside the
/// profile dir. Returns the bytes reclaimed. Only ever touches paths
/// beneath `profile_dir`.
pub fn purge_caches(profile_dir: &Path) -> Result<u64> {
    let mut reclaimed = 0u64;
    for (rel, bytes) in scan_regenerable(profile_dir)? {
        let target = profile_dir.join(&rel);
        let meta = std::fs::symlink_metadata(&target)?;
        if meta.is_dir() {
            std::fs::remove_dir_all(&target)?;
        } else {
            std::fs::remove_file(&target)?;
        }
        reclaimed += bytes;
    }
    Ok(reclaimed)
}

fn walk(dir: &Path, rel: &Path, size: &mut ProfileSize) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let child_rel = rel.join(entry.file_name());

        if crate::cache::is_regenerable(&child_rel) {
            size.cache_bytes += subtree_size(&entry.path())?;
        } else if file_type.is_dir() {
            walk(&entry.path(), &child_rel, size)?;
        } else if file_type.is_file() {
            // DirEntry::metadata is lstat-based: symlink targets are never
            // followed, so cycles are impossible and links count 0 bytes.
            size.core_bytes += entry.metadata()?.len();
        }
    }
    Ok(())
}

fn scan_inner(dir: &Path, rel: &Path, found: &mut Vec<(PathBuf, u64)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let child_rel = rel.join(entry.file_name());
        if crate::cache::is_regenerable(&child_rel) {
            let bytes = subtree_size(&entry.path())?;
            found.push((child_rel, bytes));
        } else if entry.file_type()?.is_dir() {
            scan_inner(&entry.path(), &child_rel, found)?;
        }
    }
    Ok(())
}

/// Total byte size of a file or directory tree; symlinks count 0.
fn subtree_size(path: &Path) -> Result<u64> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut total = 0u64;
        for entry in std::fs::read_dir(path)? {
            total += subtree_size(&entry?.path())?;
        }
        Ok(total)
    } else if meta.is_file() {
        Ok(meta.len())
    } else {
        Ok(0)
    }
}

/// Human-readable size in 1024-based units with one decimal (KB/MB/GB).
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn seed_profile(root: &Path) {
        fs::create_dir_all(root.join("Default")).unwrap();
        fs::write(root.join("Default/Cookies"), vec![0u8; 1000]).unwrap();
        fs::write(root.join("Default/Preferences"), vec![0u8; 500]).unwrap();
        fs::create_dir_all(root.join("Cache")).unwrap();
        fs::write(root.join("Cache/data_0"), vec![0u8; 4000]).unwrap();
        fs::create_dir_all(root.join("Default/GPUCache")).unwrap();
        fs::write(root.join("Default/GPUCache/blob"), vec![0u8; 2000]).unwrap();
        fs::create_dir_all(root.join("Default/Service Worker/CacheStorage")).unwrap();
        fs::write(
            root.join("Default/Service Worker/CacheStorage/x"),
            vec![0u8; 8000],
        )
        .unwrap();
        fs::write(root.join("OptimizationHints"), vec![0u8; 1000]).unwrap();
        // Deeper cache content must be counted with its boundary entry.
        fs::create_dir_all(root.join("Cache/Cache_Data")).unwrap();
        fs::write(root.join("Cache/Cache_Data/f"), vec![0u8; 2000]).unwrap();
    }

    #[test]
    fn measure_splits_cache_and_core() {
        let tmp = tempfile::tempdir().unwrap();
        seed_profile(tmp.path());

        let size = measure(tmp.path()).unwrap();
        // Cache: Cache (4000+2000) + Default/GPUCache (2000) + SW/CacheStorage (8000) + OptimizationHints (1000)
        assert_eq!(size.cache_bytes, 17_000);
        assert_eq!(size.core_bytes, 1_500);
        assert_eq!(size.total_bytes, 18_500);
    }

    #[test]
    fn scan_reports_top_boundary_entries_and_mutates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        seed_profile(tmp.path());
        let before = measure(tmp.path()).unwrap();

        let found = scan_regenerable(tmp.path()).unwrap();
        let rels: Vec<String> = found
            .iter()
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        assert!(rels.contains(&"Cache".to_string()));
        assert!(rels.contains(&"Default/GPUCache".to_string()));
        assert!(rels.contains(&"Default/Service Worker/CacheStorage".to_string()));
        assert!(rels.contains(&"OptimizationHints".to_string()));
        assert_eq!(rels.len(), 4, "boundary entries only, not their contents");

        // Dry-run semantics: the scan leaves everything in place.
        let after = measure(tmp.path()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn purge_removes_only_regenerable_state() {
        let tmp = tempfile::tempdir().unwrap();
        seed_profile(tmp.path());

        let reclaimed = purge_caches(tmp.path()).unwrap();
        assert_eq!(reclaimed, 17_000);

        assert!(!tmp.path().join("Cache").exists());
        assert!(!tmp.path().join("Default/GPUCache").exists());
        assert!(!tmp
            .path()
            .join("Default/Service Worker/CacheStorage")
            .exists());
        assert!(!tmp.path().join("OptimizationHints").exists());
        // Core data untouched, including the now-empty Service Worker parent.
        assert!(tmp.path().join("Default/Cookies").exists());
        assert!(tmp.path().join("Default/Preferences").exists());
        assert!(tmp.path().join("Default/Service Worker").exists());
        assert_eq!(measure(tmp.path()).unwrap().cache_bytes, 0);
    }

    #[test]
    fn empty_profile_measures_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let size = measure(tmp.path()).unwrap();
        assert_eq!(size, ProfileSize::default());
        assert_eq!(purge_caches(tmp.path()).unwrap(), 0);
    }

    #[test]
    fn format_bytes_uses_readable_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1500), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
