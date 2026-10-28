//! Scoped session and cookie export (`bm session export`).
//!
//! Provides strictly scoped, domain-filtered cookie extraction from Chromium
//! SQLite databases with OS Keychain / Secret Service decryption.
//!
//! Security Red Lines:
//! - Mandatory domain filter (no wildcard full dumps like '*' or '%')
//! - Terminal display masks sensitive cookie values by default
//! - Export files strictly enforced with 0600 permissions
//! - Optional age passphrase encryption for secure transport

use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::StorageManager;

/// A decrypted, domain-scoped cookie entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportedCookie {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub value: String,
    pub expires_utc: i64,
    pub is_secure: bool,
    pub is_httponly: bool,
    pub samesite: i32,
}

impl ExportedCookie {
    /// Mask the cookie value for safe terminal rendering.
    pub fn masked_value(&self) -> String {
        if self.value.len() <= 6 {
            format!("*** ({} bytes)", self.value.len())
        } else {
            let prefix = &self.value[..3];
            format!("{prefix}*** ({} bytes)", self.value.len())
        }
    }
}

/// A collection of exported cookies for a specific domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBundle {
    pub version: u32,
    pub browser: Browser,
    pub profile: String,
    pub domain: String,
    pub exported_at: DateTime<Utc>,
    pub cookies: Vec<ExportedCookie>,
}

/// Validate that a domain filter is non-empty and contains no wildcards.
pub fn validate_domain(domain: &str) -> Result<String> {
    let trimmed = domain.trim();
    if trimmed.is_empty()
        || trimmed == "*"
        || trimmed.contains('*')
        || trimmed.contains('%')
        || trimmed == "."
        || trimmed.chars().all(|c| c == '.')
    {
        return Err(BmError::Other(
            "Domain filter is mandatory and wildcards ('*', '%') are prohibited. Specify a concrete domain, e.g. --domain github.com".into(),
        ));
    }
    // Normalize to lowercase
    Ok(trimmed.to_lowercase())
}

/// Locate the SQLite cookie database for a given profile.
pub fn locate_cookie_db(profile_dir: &Path) -> Result<PathBuf> {
    let candidates = [
        profile_dir.join("Network").join("Cookies"),
        profile_dir.join("Cookies"),
        profile_dir.join("Default").join("Network").join("Cookies"),
        profile_dir.join("Default").join("Cookies"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }

    Err(BmError::Other(format!(
        "No Cookies database found under profile directory: {}",
        profile_dir.display()
    )))
}

/// Retrieve the Chromium Safe Storage master password from the OS.
pub fn get_os_master_password(browser: Browser) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let service_name = match browser {
            Browser::Chrome => "Chrome Safe Storage",
            Browser::Edge => "Microsoft Edge Safe Storage",
            Browser::Brave => "Brave Safe Storage",
            Browser::Chromium => "Chromium Safe Storage",
        };

        let output = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-w", "-s", service_name])
            .output()
            .map_err(|e| BmError::Other(format!("Failed to execute /usr/bin/security: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BmError::Other(format!(
                "Failed to retrieve Safe Storage key from macOS Keychain for {}: {}",
                service_name,
                stderr.trim()
            )));
        }

        let password = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if password.is_empty() {
            return Err(BmError::Other(format!(
                "Empty Safe Storage master key returned from macOS Keychain for {service_name}"
            )));
        }
        Ok(password)
    }

    #[cfg(target_os = "linux")]
    {
        let service_label = match browser {
            Browser::Chrome => "Chrome Safe Storage",
            Browser::Edge => "Microsoft Edge Safe Storage",
            Browser::Brave => "Brave Safe Storage",
            Browser::Chromium => "Chromium Safe Storage",
        };

        if let Ok(output) = std::process::Command::new("secret-tool")
            .args(["lookup", "application", service_label])
            .output()
        {
            if output.status.success() {
                let pass = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !pass.is_empty() {
                    return Ok(pass);
                }
            }
        }

        // Standard Chromium fallback password on Linux when secret-service is unavailable
        Ok("peanuts".to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = browser;
        Err(BmError::Other(
            "Session cookie export is currently supported on macOS and Linux only".into(),
        ))
    }
}

/// Derive the 128-bit AES key using PBKDF2-HMAC-SHA1.
pub fn derive_key(password: &str, iterations: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", iterations, &mut key);
    key
}

/// Decrypt a Chromium cookie payload using AES-128-CBC.
pub fn decrypt_cookie_value(encrypted_val: &[u8], key: &[u8; 16]) -> Result<String> {
    if encrypted_val.is_empty() {
        return Ok(String::new());
    }

    // Strip 'v10' or 'v11' prefix if present
    let ciphertext = if encrypted_val.starts_with(b"v10") || encrypted_val.starts_with(b"v11") {
        &encrypted_val[3..]
    } else {
        encrypted_val
    };

    if ciphertext.is_empty() {
        return Ok(String::new());
    }

    let iv = [0x20u8; 16]; // 16 ASCII spaces
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let decryptor = Aes128CbcDec::new(key.into(), &iv.into());
    let decrypted = decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|e| BmError::Other(format!("Failed to decrypt cookie payload: {e}")))?;

    String::from_utf8(decrypted)
        .map_err(|e| BmError::Other(format!("Decrypted cookie is not valid UTF-8: {e}")))
}

/// Encrypt a mock cookie for testing purposes.
#[doc(hidden)]
#[allow(dead_code)]
pub fn encrypt_mock_cookie(plaintext: &str, key: &[u8; 16]) -> Vec<u8> {
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    let iv = [0x20u8; 16];
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let encryptor = Aes128CbcEnc::new(key.into(), &iv.into());
    let ciphertext = encryptor.encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
    let mut out = b"v10".to_vec();
    out.extend(ciphertext);
    out
}

/// Extract and decrypt cookies strictly scoped to the specified domain.
pub fn extract_scoped_cookies(
    db_path: &Path,
    domain_filter: &str,
    key: &[u8; 16],
) -> Result<Vec<ExportedCookie>> {
    let clean_domain = validate_domain(domain_filter)?;
    let dot_domain = if clean_domain.starts_with('.') {
        clean_domain.clone()
    } else {
        format!(".{clean_domain}")
    };
    let bare_domain = clean_domain.trim_start_matches('.').to_string();
    let like_subdomain = format!("%.{bare_domain}");

    // Copy DB to a temporary folder to avoid WAL concurrency locks
    let tmp_dir = tempfile::tempdir()?;
    let tmp_db = tmp_dir.path().join("Cookies");
    fs::copy(db_path, &tmp_db)?;

    // Also copy WAL/SHM if present
    let wal = db_path.with_file_name("Cookies-wal");
    if wal.exists() {
        let _ = fs::copy(&wal, tmp_dir.path().join("Cookies-wal"));
    }
    let shm = db_path.with_file_name("Cookies-shm");
    if shm.exists() {
        let _ = fs::copy(&shm, tmp_dir.path().join("Cookies-shm"));
    }

    let conn = Connection::open_with_flags(
        &tmp_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| BmError::Other(format!("Failed to open Cookies database: {e}")))?;

    let mut stmt = conn
        .prepare(
            "SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure, is_httponly, samesite \
             FROM cookies \
             WHERE host_key = ?1 OR host_key = ?2 OR host_key LIKE ?3 \
             ORDER BY host_key, name",
        )
        .map_err(|e| BmError::Other(format!("Failed to query cookies table: {e}")))?;

    struct RawRow {
        host_key: String,
        name: String,
        value: String,
        encrypted_value: Vec<u8>,
        path: String,
        expires_utc: i64,
        is_secure: bool,
        is_httponly: bool,
        samesite: i32,
    }

    let rows = stmt
        .query_map(
            rusqlite::params![bare_domain, dot_domain, like_subdomain],
            |row| {
                Ok(RawRow {
                    host_key: row.get(0)?,
                    name: row.get(1)?,
                    value: row.get(2)?,
                    encrypted_value: row.get(3)?,
                    path: row.get(4)?,
                    expires_utc: row.get(5)?,
                    is_secure: row.get::<_, i64>(6)? != 0,
                    is_httponly: row.get::<_, i64>(7)? != 0,
                    samesite: row.get(8)?,
                })
            },
        )
        .map_err(|e| BmError::Other(format!("Failed to execute cookie query: {e}")))?;

    let mut cookies = Vec::new();

    for row_res in rows {
        let r = row_res.map_err(|e| BmError::Other(format!("Failed to read cookie row: {e}")))?;

        // Defense-in-depth: Verify domain matches target or subdomain
        let host = r.host_key.to_lowercase();
        let matches =
            host == bare_domain || host == dot_domain || host.ends_with(&format!(".{bare_domain}"));

        if !matches {
            continue;
        }

        let final_value = if !r.encrypted_value.is_empty() {
            decrypt_cookie_value(&r.encrypted_value, key)?
        } else {
            r.value
        };

        cookies.push(ExportedCookie {
            name: r.name,
            domain: r.host_key,
            path: r.path,
            value: final_value,
            expires_utc: r.expires_utc,
            is_secure: r.is_secure,
            is_httponly: r.is_httponly,
            samesite: r.samesite,
        });
    }

    Ok(cookies)
}

/// Export cookies for a profile to a `SessionBundle`.
pub fn export_session(
    storage: &StorageManager,
    browser: Browser,
    profile: &str,
    domain: &str,
) -> Result<SessionBundle> {
    StorageManager::validate_name(profile)?;
    let clean_domain = validate_domain(domain)?;

    let profile_dir = storage.profile_dir(browser, profile);
    if !profile_dir.exists() {
        return Err(BmError::ProfileNotFound {
            name: profile.to_string(),
            browser: browser.display_name().to_string(),
        });
    }

    let cookie_db = locate_cookie_db(&profile_dir)?;
    let master_pass = get_os_master_password(browser)?;

    // macOS uses 1003 iterations; Linux uses 1
    #[cfg(target_os = "macos")]
    let iterations = 1003;
    #[cfg(not(target_os = "macos"))]
    let iterations = 1;

    let key = derive_key(&master_pass, iterations);
    let cookies = extract_scoped_cookies(&cookie_db, &clean_domain, &key)?;

    Ok(SessionBundle {
        version: 1,
        browser,
        profile: profile.to_string(),
        domain: clean_domain,
        exported_at: Utc::now(),
        cookies,
    })
}

/// Save a SessionBundle to a file with strict 0600 permissions, optionally age-encrypted.
pub fn save_session_bundle(
    bundle: &SessionBundle,
    dest: &Path,
    passphrase: Option<&str>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let json_bytes = serde_json::to_vec_pretty(bundle)?;

    let payload = match passphrase {
        Some(pass) => {
            // Encrypt using age passphrase
            let secret = age::secrecy::SecretString::from(pass.to_string());
            let encryptor = age::Encryptor::with_user_passphrase(secret);
            let mut encrypted = Vec::new();
            let mut writer = encryptor
                .wrap_output(&mut encrypted)
                .map_err(|e| BmError::Other(format!("Failed to initialize age encryption: {e}")))?;
            writer.write_all(&json_bytes)?;
            writer
                .finish()
                .map_err(|e| BmError::Other(format!("Failed to finalize age encryption: {e}")))?;
            encrypted
        }
        None => json_bytes,
    };

    let tmp = dest.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o600);
        let _ = file.set_permissions(perms);
    }
    file.write_all(&payload)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_validation_rejects_wildcards_and_empty() {
        assert!(validate_domain("github.com").is_ok());
        assert!(validate_domain(".google.com").is_ok());
        assert!(validate_domain("sub.domain.co.uk").is_ok());

        assert!(validate_domain("").is_err());
        assert!(validate_domain("   ").is_err());
        assert!(validate_domain("*").is_err());
        assert!(validate_domain("*.github.com").is_err());
        assert!(validate_domain("github.*").is_err());
        assert!(validate_domain("%").is_err());
        assert!(validate_domain(".").is_err());
        assert!(validate_domain("...").is_err());
    }

    #[test]
    fn test_cookie_encryption_decryption_roundtrip() {
        let password = "test-master-password";
        let key = derive_key(password, 1003);

        let secret_value = "gho_1234567890abcdefghijklmnopqrstuvwxyz";
        let encrypted = encrypt_mock_cookie(secret_value, &key);

        let decrypted = decrypt_cookie_value(&encrypted, &key).unwrap();
        assert_eq!(decrypted, secret_value);
    }

    #[test]
    fn test_scoped_sqlite_extraction() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("Cookies");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE cookies (
                host_key TEXT NOT NULL,
                name TEXT NOT NULL,
                value TEXT NOT NULL,
                encrypted_value BLOB DEFAULT '',
                path TEXT NOT NULL,
                expires_utc INTEGER NOT NULL,
                is_secure INTEGER NOT NULL,
                is_httponly INTEGER NOT NULL,
                samesite INTEGER NOT NULL DEFAULT -1
            )",
            [],
        )
        .unwrap();

        let key = derive_key("test-key", 1);
        let val1_enc = encrypt_mock_cookie("secret_session_1", &key);
        let val2_enc = encrypt_mock_cookie("secret_token_2", &key);
        let val3_enc = encrypt_mock_cookie("other_site_cookie", &key);

        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, '', ?3, '/', 1700000000, 1, 1, 1)",
            rusqlite::params![".example.com", "session_id", val1_enc],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, '', ?3, '/', 1700000000, 1, 0, 0)",
            rusqlite::params!["auth.example.com", "auth_token", val2_enc],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, '', ?3, '/', 1700000000, 1, 1, 1)",
            rusqlite::params![".unrelated.org", "unrelated_cookie", val3_enc],
        )
        .unwrap();

        // 1. Scoped query for example.com
        let extracted = extract_scoped_cookies(&db_path, "example.com", &key).unwrap();
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0].name, "session_id");
        assert_eq!(extracted[0].value, "secret_session_1");
        assert_eq!(extracted[1].name, "auth_token");
        assert_eq!(extracted[1].value, "secret_token_2");

        // Verify masking
        assert!(extracted[0].masked_value().starts_with("sec***"));

        // 2. Unrelated site is completely excluded
        assert!(extracted.iter().all(|c| c.domain.contains("example.com")));
    }

    #[test]
    fn test_save_session_bundle_permissions_and_age_encryption() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_file = tmp.path().join("session.json");

        let bundle = SessionBundle {
            version: 1,
            browser: Browser::Chrome,
            profile: "test-profile".into(),
            domain: "example.com".into(),
            exported_at: Utc::now(),
            cookies: vec![ExportedCookie {
                name: "sid".into(),
                domain: ".example.com".into(),
                path: "/".into(),
                value: "super_secret".into(),
                expires_utc: 0,
                is_secure: true,
                is_httponly: true,
                samesite: 1,
            }],
        };

        // Plain save
        save_session_bundle(&bundle, &bundle_file, None).unwrap();
        #[cfg(unix)]
        {
            let meta = fs::metadata(&bundle_file).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        // Age encrypted save
        let enc_file = tmp.path().join("session.age");
        save_session_bundle(&bundle, &enc_file, Some("my-age-passphrase")).unwrap();
        #[cfg(unix)]
        {
            let meta = fs::metadata(&enc_file).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        // Verify the file contains age header
        let content = fs::read(&enc_file).unwrap();
        assert!(content.starts_with(b"age-encryption.org/"));
    }
}
