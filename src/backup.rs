//! Local profile snapshots (`bm backup` / `bm restore`).
//!
//! Archives live under `~/.bm/backups` as
//! `<browser>-<name>-<YYYYmmddHHMMSS>.tar.zst`, or
//! `<browser>-<name>-<YYYYmmddHHMMSS>.tar.zst.age` when encrypted. A
//! `manifest.json` entry is embedded at backup time so a restore can
//! register the profile's description and egress proxy without asking
//! again. Regenerable caches are excluded unless `--full` (see src/cache.rs).
//! This is intentionally a local-machine feature — no cross-machine sync.
//!
//! Encryption (`--encrypt`): age v1 with a scrypt passphrase recipient
//! (tar → zstd → age layering). The passphrase comes from
//! `BM_BACKUP_PASSPHRASE` or a hidden interactive prompt — never argv.
//! Threat model, stated plainly: bm does NO key management. The passphrase
//! is the only key; a lost passphrase means a lost backup, by design. The
//! age format authenticates the header, so a wrong passphrase (or a
//! tampered archive) fails decryption instead of yielding silent garbage.

use std::collections::hash_map::Entry;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use age::secrecy::SecretString;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::{ProfileMetadata, StorageManager};

const MANIFEST_ENTRY: &str = "manifest.json";
/// Environment variable supplying the backup passphrase non-interactively.
pub const PASSPHRASE_ENV: &str = "BM_BACKUP_PASSPHRASE";
const PLAIN_SUFFIX: &str = ".tar.zst";
const ENCRYPTED_SUFFIX: &str = ".tar.zst.age";

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

/// One row of a `bm backup --all` run.
#[derive(Debug)]
pub enum AllBackupOutcome {
    BackedUp { name: String, outcome: BackupOutcome },
    /// Profile skipped because a live browser process holds it.
    SkippedRunning { name: String, pid: Option<u32> },
    /// Profile failed; the run continues with the remaining profiles.
    Failed { name: String, error: String },
}

/// Snapshot every profile of `browser`, skipping profiles held by a running
/// browser process (reported, not fatal). Per-profile failures do not abort
/// the run; the caller decides the exit code from the outcome mix. When
/// `passphrase` is set, every produced archive is encrypted with it.
pub fn create_backup_all(
    storage: &StorageManager,
    browser: Browser,
    full: bool,
    keep: Option<usize>,
    passphrase: Option<&SecretString>,
) -> Result<Vec<AllBackupOutcome>> {
    let profiles: Vec<String> = storage
        .list_profiles(browser)?
        .into_iter()
        .map(|(m, _)| m.name)
        .collect();
    let mut outcomes = Vec::new();
    for name in profiles {
        let dir = storage.profile_dir(browser, &name);
        if let Some(pid) = crate::running::running_pid(&dir)? {
            outcomes.push(AllBackupOutcome::SkippedRunning { name, pid: Some(pid) });
            continue;
        }
        match create_backup_unchecked(storage, browser, &name, full, keep, passphrase) {
            Ok(outcome) => outcomes.push(AllBackupOutcome::BackedUp { name, outcome }),
            Err(e) => outcomes.push(AllBackupOutcome::Failed { name, error: e.to_string() }),
        }
    }
    Ok(outcomes)
}

fn backups_dir(storage: &StorageManager) -> PathBuf {
    storage.base_dir().join("backups")
}

/// Whether the archive filename marks it as age-encrypted.
pub fn is_encrypted_archive(archive: &Path) -> bool {
    archive
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(ENCRYPTED_SUFFIX))
        .unwrap_or(false)
}

/// Resolve the passphrase from `BM_BACKUP_PASSPHRASE`, falling back to a
/// hidden prompt. `confirm` asks twice (creating a backup); decryption asks
/// once. Never reaches argv; non-interactive runs without the env var fail
/// with instructions instead of hanging.
pub fn read_passphrase(confirm: bool) -> Result<SecretString> {
    use std::io::IsTerminal;
    if let Ok(value) = std::env::var(PASSPHRASE_ENV) {
        if !value.is_empty() {
            return Ok(SecretString::from(value));
        }
    }
    if !std::io::stdin().is_terminal() {
        return Err(BmError::Other(format!(
            "no TTY available for the passphrase prompt — set {PASSPHRASE_ENV} for non-interactive runs"
        )));
    }
    let prompt = if confirm {
        inquire::Password::new("Backup passphrase:")
    } else {
        inquire::Password::new("Backup passphrase:").without_confirmation()
    };
    let value = prompt
        .prompt()
        .map_err(|e| BmError::Other(format!("passphrase prompt failed: {e}")))?;
    if value.is_empty() {
        return Err(BmError::Other("empty passphrase refused".into()));
    }
    Ok(SecretString::from(value))
}

/// The passphrase needed to open `archive`: resolved only when the filename
/// says the archive is encrypted, so plain backups never prompt.
pub fn passphrase_to_open(archive: &Path) -> Result<Option<SecretString>> {
    if is_encrypted_archive(archive) {
        Ok(Some(read_passphrase(false)?))
    } else {
        Ok(None)
    }
}

/// Create a snapshot of `name`. Refuses while the browser is running.
pub fn create_backup(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    full: bool,
    keep: Option<usize>,
    passphrase: Option<&SecretString>,
) -> Result<BackupOutcome> {
    if browser.is_running() {
        return Err(BmError::BrowserRunning {
            browser: browser.display_name().to_string(),
        });
    }
    create_backup_unchecked(storage, browser, name, full, keep, passphrase)
}

/// Backup without the running-browser guard; separate so tests can run
/// against `is_running`'s process-table dependency.
pub fn create_backup_unchecked(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    full: bool,
    keep: Option<usize>,
    passphrase: Option<&SecretString>,
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
    let suffix = if passphrase.is_some() { ENCRYPTED_SUFFIX } else { PLAIN_SUFFIX };
    let path = dir.join(format!("{browser}-{name}-{timestamp}{suffix}"));
    // Write through a .tmp name so a killed backup never leaves a
    // half-written archive that looks complete.
    let tmp_path = dir.join(format!(
        "{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    ));

    let profile_bytes = crate::size::measure(&profile_dir)?.total_bytes;
    let archive_bytes = match write_archive(storage, browser, name, &profile_dir, &tmp_path, full, passphrase) {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }
    };
    fs::rename(&tmp_path, &path)?;

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

/// Output sink for a backup archive: plain file, or age stream writer whose
/// `finish()` must run after the inner zstd frame is flushed.
enum ArchiveSink {
    Plain(fs::File),
    Encrypted(age::stream::StreamWriter<fs::File>),
}

impl Write for ArchiveSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            ArchiveSink::Plain(f) => f.write(buf),
            ArchiveSink::Encrypted(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            ArchiveSink::Plain(f) => f.flush(),
            ArchiveSink::Encrypted(w) => w.flush(),
        }
    }
}

impl ArchiveSink {
    /// Finalize the outermost layer (the age trailer, when encrypted).
    fn finish(self) -> std::io::Result<()> {
        match self {
            ArchiveSink::Plain(_) => Ok(()),
            ArchiveSink::Encrypted(w) => w.finish().map(|_| ()),
        }
    }
}

fn write_archive(
    storage: &StorageManager,
    browser: Browser,
    name: &str,
    profile_dir: &Path,
    out_path: &Path,
    full: bool,
    passphrase: Option<&SecretString>,
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
    // Layering, outermost first: age (optional) → zstd → tar.
    let sink = match passphrase {
        Some(p) => {
            let encryptor = age::Encryptor::with_user_passphrase(p.clone());
            ArchiveSink::Encrypted(encryptor.wrap_output(file)?)
        }
        None => ArchiveSink::Plain(file),
    };
    let encoder = zstd::Encoder::new(sink, 3)?;
    let mut tar = tar::Builder::new(encoder);

    // Manifest first so a streaming restore can read it immediately.
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, MANIFEST_ENTRY, manifest_json.as_slice())?;

    append_tree(&mut tar, profile_dir, Path::new(""), !full)?;

    let encoder = tar.into_inner()?;
    let sink = encoder.finish()?;
    sink.finish()
        .map_err(|e| BmError::Other(format!("failed to finalize archive: {e}")))?;
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
/// were deleted. Timestamps in the filename sort lexicographically. Both
/// plain and encrypted (.age) archives count toward the same per-profile
/// retention pool.
pub fn prune_old_backups(dir: &Path, browser: Browser, name: &str, keep: usize) -> Result<usize> {
    let prefix = format!("{browser}-{name}-");
    let mut archives: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && (n.ends_with(PLAIN_SUFFIX) || n.ends_with(ENCRYPTED_SUFFIX)))
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
/// running. Encrypted archives (.age) are decrypted with `passphrase` on the
/// fly.
pub fn restore_archive(
    storage: &StorageManager,
    archive: &Path,
    rename_to: Option<&str>,
    force: bool,
    passphrase: Option<&SecretString>,
) -> Result<ProfileMetadata> {
    let manifest = peek_manifest(archive, passphrase)?;
    let browser = Browser::deserialize_manifest_browser(&manifest.browser)?;
    if browser.is_running() {
        return Err(BmError::BrowserRunning {
            browser: browser.display_name().to_string(),
        });
    }
    restore_archive_unchecked(storage, archive, rename_to, force, passphrase)
}

/// Restore without the running-browser guard; separate so tests can run
/// against `is_running`'s process-table dependency.
pub fn restore_archive_unchecked(
    storage: &StorageManager,
    archive: &Path,
    rename_to: Option<&str>,
    force: bool,
    passphrase: Option<&SecretString>,
) -> Result<ProfileMetadata> {
    let manifest = peek_manifest(archive, passphrase)?;
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

    extract_archive(archive, &dest, passphrase)?;

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

/// Open an archive for reading, decrypting age-encrypted files on the fly.
/// A passphrase must be supplied for encrypted archives; a wrong passphrase
/// (or tampered file) fails authentication with a clear error — the age
/// format never yields silent garbage.
fn open_archive_reader(
    archive: &Path,
    passphrase: Option<&SecretString>,
) -> Result<Box<dyn Read>> {
    let file = fs::File::open(archive)
        .map_err(|e| BmError::Other(format!("cannot open archive {}: {e}", archive.display())))?;
    if !is_encrypted_archive(archive) {
        return Ok(Box::new(file));
    }
    let Some(p) = passphrase else {
        return Err(BmError::Other(format!(
            "{} is encrypted — provide the passphrase (BM_BACKUP_PASSPHRASE or interactive prompt)",
            archive.display()
        )));
    };
    let decryptor = age::Decryptor::new(file).map_err(|e| {
        BmError::Other(format!("{} is not a valid age-encrypted archive: {e}", archive.display()))
    })?;
    let identity = age::scrypt::Identity::new(p.clone());
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| {
            BmError::Other(format!(
                "cannot decrypt {} — wrong passphrase or the archive is corrupted",
                archive.display()
            ))
        })?;
    Ok(Box::new(reader))
}

/// Parse the embedded manifest from an archive file.
fn peek_manifest(archive: &Path, passphrase: Option<&SecretString>) -> Result<BackupManifest> {
    let reader = open_archive_reader(archive, passphrase)?;
    let decoder = zstd::Decoder::new(reader)?;
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

/// Result of validating an archive with `bm backup --verify`.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// Profile the archive was taken from.
    pub name: String,
    /// Browser the archive belongs to (as stored in the manifest).
    pub browser: String,
    pub created_at: DateTime<Utc>,
    /// The archived proxy, if the profile carried one.
    pub proxy: Option<crate::proxy::ProxyConfig>,
    /// bm version that created the archive.
    pub bm_version: String,
    /// Number of entries (files, dirs, symlinks) the tar carries.
    pub entries: usize,
    /// Total decoded (uncompressed) byte count — proof the zstd frame and
    /// every tar payload decode end to end.
    pub decoded_bytes: u64,
}

/// Validate an archive without extracting it: the zstd frame must decode in
/// full, every tar entry header must be readable, and the embedded manifest
/// must parse and carry the expected fields. Encrypted archives are
/// decrypted in-stream first (wrong passphrase = validation failure). Any
/// corruption aborts with a non-zero exit and a specific message.
pub fn verify_archive(archive: &Path, passphrase: Option<&SecretString>) -> Result<VerifyReport> {
    let reader = if is_encrypted_archive(archive) {
        open_archive_reader(archive, passphrase)?
    } else {
        // The streaming decoder only surfaces a bad magic on first read, so
        // check the frame magic up front for a precise "not a backup" error.
        let mut file = fs::File::open(archive).map_err(|e| {
            BmError::Other(format!("cannot open archive {}: {e}", archive.display()))
        })?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(|e| {
            BmError::Other(format!("{} is too short to be a backup: {e}", archive.display()))
        })?;
        if magic != [0x28, 0xB5, 0x2F, 0xFD] {
            return Err(BmError::Other(format!(
                "{} is not a zstd archive (unexpected magic bytes)",
                archive.display()
            )));
        }
        file.rewind()?;
        Box::new(file) as Box<dyn Read>
    };

    let decoder = zstd::Decoder::new(reader)?;
    let mut tar = tar::Archive::new(decoder);

    let mut manifest = None;
    let mut entries = 0usize;
    let mut decoded_bytes = 0u64;
    // Iterating through every entry exercises the full zstd frame: with a
    // streaming (non-seekable) input, advancing past an entry requires
    // reading its payload.
    let iter = tar.entries()?;
    for entry in iter {
        let mut entry = entry
            .map_err(|e| BmError::Other(format!("corrupt archive {}: {e}", archive.display())))?;
        entries += 1;
        decoded_bytes += entry.header().size()?;

        if entry.path()? == Path::new(MANIFEST_ENTRY) {
            let mut buf = String::new();
            entry.read_to_string(&mut buf).map_err(|e| {
                BmError::Other(format!(
                    "cannot read {MANIFEST_ENTRY} in {}: {e}",
                    archive.display()
                ))
            })?;
            let parsed: BackupManifest = serde_json::from_str(&buf).map_err(|e| {
                BmError::Other(format!(
                    "manifest in {} does not parse: {e}",
                    archive.display()
                ))
            })?;
            validate_manifest(&parsed, archive)?;
            manifest = Some(parsed);
        }
    }

    let manifest = manifest.ok_or_else(|| {
        BmError::Other(format!(
            "no {MANIFEST_ENTRY} in {} — not a bm backup",
            archive.display()
        ))
    })?;

    Ok(VerifyReport {
        name: manifest.name,
        browser: manifest.browser,
        created_at: manifest.created_at,
        proxy: manifest.proxy,
        bm_version: manifest.bm_version,
        entries,
        decoded_bytes,
    })
}

/// The manifest must carry the fields a restore depends on; the archive is
/// useless when any of them is missing or out of range.
fn validate_manifest(manifest: &BackupManifest, archive: &Path) -> Result<()> {
    if manifest.name.is_empty() {
        return Err(BmError::Other(format!(
            "manifest in {} has an empty profile name",
            archive.display()
        )));
    }
    // The browser must map onto a browser bm knows (same table restore uses).
    Browser::deserialize_manifest_browser(&manifest.browser)?;
    if manifest.bm_version.is_empty() {
        return Err(BmError::Other(format!(
            "manifest in {} is missing the creating bm version",
            archive.display()
        )));
    }
    Ok(())
}

/// Extract all entries below `dest`, rejecting path traversal (absolute
/// paths, `..`, prefixes) so a hostile archive cannot escape the profile dir.
fn extract_archive(
    archive: &Path,
    dest: &Path,
    passphrase: Option<&SecretString>,
) -> Result<()> {
    let reader = open_archive_reader(archive, passphrase)?;
    let decoder = zstd::Decoder::new(reader)?;
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
            create_backup_unchecked(&storage, browser, "work", false, None, None).unwrap();
        assert!(outcome.path.exists());
        assert!(outcome.archive_bytes > 0);
        assert!(outcome.profile_bytes > 0);

        // Restore under a new name (renamed restore).
        let meta = restore_archive_unchecked(
            &storage,
            &outcome.path,
            Some("work-restored"),
            false,
            None,
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

        let outcome = create_backup_unchecked(&storage, browser, "work", true, None, None).unwrap();
        let meta = restore_archive_unchecked(&storage, &outcome.path, Some("full"), false, None).unwrap();
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
        let outcome = create_backup_unchecked(&storage, browser, "work", false, None, None).unwrap();

        assert!(restore_archive_unchecked(&storage, &outcome.path, None, false, None).is_err());
        restore_archive_unchecked(&storage, &outcome.path, None, true, None).unwrap();
        // Existing dir was replaced by the archive content.
        assert_eq!(
            fs::read_to_string(storage.profile_dir(browser, "work/Default/Cookies")).unwrap(),
            "cookie-db"
        );
    }

    #[test]
    fn backup_all_skips_running_profiles_and_snapshots_the_rest() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "busy");
        seed(&storage, browser, "idle");

        // "busy" is held by a live process — this test process itself.
        std::os::unix::fs::symlink(
            format!("host-{}", std::process::id()),
            storage.profile_dir(browser, "busy").join("SingletonLock"),
        )
        .unwrap();

        let outcomes = create_backup_all(&storage, browser, false, None, None).unwrap();
        assert_eq!(outcomes.len(), 2);

        let mut backed_up = None;
        for outcome in outcomes {
            match outcome {
                AllBackupOutcome::SkippedRunning { name, pid } => {
                    assert_eq!(name, "busy");
                    assert_eq!(pid, Some(std::process::id()));
                    let archived_busy = fs::read_dir(backups_dir(&storage))
                        .map(|entries| entries.flatten().any(|e| {
                            e.file_name().to_string_lossy().starts_with("chrome-busy-")
                        }))
                        .unwrap_or(false);
                    assert!(!archived_busy, "running profile must not be archived");
                }
                AllBackupOutcome::BackedUp { name, outcome } => {
                    assert_eq!(name, "idle");
                    assert!(outcome.path.exists());
                    backed_up = Some(outcome);
                }
                AllBackupOutcome::Failed { name, error } => {
                    panic!("unexpected failure for {name}: {error}");
                }
            }
        }
        assert!(backed_up.is_some(), "the stopped profile must be backed up");
    }

    #[test]
    fn backup_all_with_no_profiles_is_empty_not_an_error() {
        let (_tmp, storage) = setup();
        let outcomes = create_backup_all(&storage, Browser::Chrome, false, None, None).unwrap();
        assert!(outcomes.is_empty());
    }

    #[test]
    fn verify_accepts_a_valid_archive_and_reports_contents() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");

        let outcome = create_backup_unchecked(&storage, browser, "work", false, None, None).unwrap();
        let report = verify_archive(&outcome.path, None).unwrap();

        assert_eq!(report.name, "work");
        assert_eq!(report.browser, "chrome");
        assert_eq!(
            report.proxy.as_ref().unwrap().server_string(),
            "socks5://10.0.0.1:1080"
        );
        assert!(!report.bm_version.is_empty());
        // manifest + Default/Cookies (+ dirs tar-rs records) — at least the manifest.
        assert!(report.entries >= 2, "expected manifest plus profile entries");
        assert!(report.decoded_bytes > 0);
    }

    #[test]
    fn verify_rejects_truncated_and_garbage_archives() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "work");
        let outcome = create_backup_unchecked(&storage, browser, "work", true, None, None).unwrap();

        // Truncated zstd frame: cut the file in half.
        let full = fs::read(&outcome.path).unwrap();
        let truncated = tempfile::tempdir().unwrap();
        let truncated_path = truncated.path().join("cut.tar.zst");
        fs::write(&truncated_path, &full[..full.len() / 2]).unwrap();
        let err = verify_archive(&truncated_path, None).unwrap_err();
        assert!(err.to_string().contains("corrupt"), "got: {err}");

        // Garbage with the right extension but no zstd magic.
        let garbage = truncated.path().join("garbage.tar.zst");
        fs::write(&garbage, b"definitely not zstd").unwrap();
        let err = verify_archive(&garbage, None).unwrap_err();
        assert!(err.to_string().contains("not a zstd archive"), "got: {err}");

        // Valid zstd frame containing a valid-but-empty tar: no manifest.
        let foreign = truncated.path().join("foreign.tar.zst");
        let mut empty_tar = Vec::new();
        tar::Builder::new(&mut empty_tar).finish().unwrap();
        fs::write(&foreign, zstd::stream::encode_all(&empty_tar[..], 3).unwrap()).unwrap();
        let err = verify_archive(&foreign, None).unwrap_err();
        assert!(err.to_string().contains("not a bm backup"), "got: {err}");

        // Valid zstd frame with non-tar garbage inside reports corruption.
        let garbage_tar = truncated.path().join("garbage-inner.tar.zst");
        fs::write(&garbage_tar, zstd::stream::encode_all(&b"hello"[..], 3).unwrap()).unwrap();
        let err = verify_archive(&garbage_tar, None).unwrap_err();
        assert!(err.to_string().contains("corrupt"), "got: {err}");

        let _ = storage;
    }

    #[test]
    fn verify_rejects_a_manifest_with_an_unknown_browser() {
        let evil = tempfile::tempdir().unwrap();
        let archive = evil.path().join("foreign.tar.zst");
        let manifest = r#"{"name":"x","browser":"safari","description":"","created_at":"2026-01-01T00:00:00Z","bm_version":"0.0.5"}"#;
        let mut raw: Vec<u8> = Vec::new();
        // Reuse the raw-ustar builder from the traversal test.
        raw.extend_from_slice(&raw_tar_header(MANIFEST_ENTRY, manifest.len()));
        raw.extend_from_slice(manifest.as_bytes());
        let pad = (512 - manifest.len() % 512) % 512;
        raw.extend_from_slice(&vec![0u8; pad]);
        raw.extend_from_slice(&[0u8; 1024]);
        fs::write(&archive, zstd::stream::encode_all(&raw[..], 3).unwrap()).unwrap();

        let err = verify_archive(&archive, None).unwrap_err();
        assert!(err.to_string().contains("safari"), "got: {err}");
    }

    #[test]
    fn encrypted_backup_roundtrip_and_wrong_passphrase_fails() {
        let (_tmp, storage) = setup();
        let browser = Browser::Chrome;
        seed(&storage, browser, "secret");

        let pass = SecretString::from("correct horse battery staple".to_string());
        let outcome =
            create_backup_unchecked(&storage, browser, "secret", false, None, Some(&pass))
                .unwrap();
        assert!(is_encrypted_archive(&outcome.path), "suffix must be .tar.zst.age");
        // The plaintext must not leak into the file.
        let raw = fs::read(&outcome.path).unwrap();
        assert!(
            !window_contains(&raw, b"cookie-db"),
            "profile content must not be readable in an encrypted archive"
        );

        // Verify and restore with the right passphrase.
        let report = verify_archive(&outcome.path, Some(&pass)).unwrap();
        assert_eq!(report.name, "secret");
        let meta = restore_archive_unchecked(
            &storage,
            &outcome.path,
            Some("decrypted"),
            false,
            Some(&pass),
        )
        .unwrap();
        assert_eq!(
            meta.proxy.as_ref().unwrap().server_string(),
            "socks5://10.0.0.1:1080"
        );
        assert_eq!(
            fs::read_to_string(storage.profile_dir(browser, "decrypted/Default/Cookies")).unwrap(),
            "cookie-db"
        );

        // Wrong passphrase: clear failure, no partial profile.
        let wrong = SecretString::from("nope".to_string());
        let err = verify_archive(&outcome.path, Some(&wrong)).unwrap_err();
        assert!(err.to_string().contains("wrong passphrase"), "got: {err}");
        let err = restore_archive_unchecked(
            &storage,
            &outcome.path,
            Some("broken"),
            false,
            Some(&wrong),
        )
        .unwrap_err();
        assert!(err.to_string().contains("wrong passphrase"), "got: {err}");
        assert!(!storage.profile_dir(browser, "broken").exists());

        // Missing passphrase on an encrypted archive: instructive error.
        let err = verify_archive(&outcome.path, None).unwrap_err();
        assert!(err.to_string().contains("encrypted"), "got: {err}");
    }

    #[test]
    fn encrypted_and_plain_archives_share_the_retention_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backups");
        fs::create_dir_all(&dir).unwrap();
        for name in [
            "chrome-work-20260101000000.tar.zst",
            "chrome-work-20260102000000.tar.zst.age",
            "chrome-work-20260103000000.tar.zst",
        ] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let removed = prune_old_backups(&dir, Browser::Chrome, "work", 2).unwrap();
        assert_eq!(removed, 1);
        assert!(!dir.join("chrome-work-20260101000000.tar.zst").exists());
        assert!(dir.join("chrome-work-20260102000000.tar.zst.age").exists());
    }

    /// Naive substring search over the raw bytes (no alignment guarantees —
    /// good enough for "did this secret leak into the ciphertext").
    fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
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

        let result = restore_archive_unchecked(&storage, &archive_path, None, false, None);
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

        let outcome = create_backup_unchecked(&storage, browser, "work", true, None, None).unwrap();
        restore_archive_unchecked(&storage, &outcome.path, Some("linked"), false, None).unwrap();
        let link = storage.profile_dir(browser, "linked/Default/link");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("Cookies"));
    }
}
