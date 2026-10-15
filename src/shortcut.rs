use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::browser::Browser;
use crate::error::{BmError, Result};
use crate::storage::StorageManager;

pub struct ShortcutGenerator;

impl ShortcutGenerator {
    /// Generate a standalone desktop shortcut (.app on macOS, shell script on Linux).
    pub fn create_shortcut(
        storage: &StorageManager,
        browser: Browser,
        profile_name: &str,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let profile_dir = storage.profile_dir(browser, profile_name);
        if !profile_dir.exists() {
            return Err(BmError::ProfileNotFound {
                name: profile_name.to_string(),
                browser: browser.display_name().to_string(),
            });
        }

        if cfg!(target_os = "macos") {
            Self::create_macos_app_bundle(browser, profile_name, &profile_dir, dest_dir)
        } else {
            Self::create_linux_script(storage, browser, profile_name, &profile_dir, dest_dir)
        }
    }

    /// Create macOS standalone .app bundle.
    fn create_macos_app_bundle(
        browser: Browser,
        profile_name: &str,
        profile_dir: &Path,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| {
            BmError::Other("Unable to resolve home directory".into())
        })?;

        let target_parent = match dest_dir {
            Some(p) => p.to_path_buf(),
            None => {
                let user_apps = home.join("Applications");
                if user_apps.exists() {
                    user_apps
                } else {
                    home.join("Desktop")
                }
            }
        };

        if !target_parent.exists() {
            fs::create_dir_all(&target_parent)?;
        }

        let prefix = match browser {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
        };

        let app_name = format!("{} ({}).app", prefix, profile_name);
        let app_dir = target_parent.join(&app_name);

        if app_dir.exists() {
            fs::remove_dir_all(&app_dir)?;
        }

        let macos_dir = app_dir.join("Contents/MacOS");
        let resources_dir = app_dir.join("Contents/Resources");
        fs::create_dir_all(&macos_dir)?;
        fs::create_dir_all(&resources_dir)?;

        let binary_path = browser.binary_path()?;
        let launcher_path = macos_dir.join("launcher");

        let script = format!(
            "#!/bin/sh\nexec \"{}\" --user-data-dir=\"{}\" --no-first-run \"$@\"\n",
            binary_path.display(),
            profile_dir.display()
        );

        fs::write(&launcher_path, script)?;
        let mut perms = fs::metadata(&launcher_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&launcher_path, perms)?;

        // Try to copy Info.plist and app icon from original application bundle
        if let Ok(orig_app) = browser.app_path() {
            let orig_plist = orig_app.join("Contents/Info.plist");
            let target_plist = app_dir.join("Contents/Info.plist");
            if orig_plist.exists() {
                let _ = fs::copy(&orig_plist, &target_plist);
                let _ = Command::new("/usr/libexec/PlistBuddy")
                    .args([
                        "-c",
                        "Set :CFBundleExecutable launcher",
                        target_plist.to_str().unwrap_or(""),
                    ])
                    .status();
                let _ = Command::new("/usr/libexec/PlistBuddy")
                    .args([
                        "-c",
                        &format!("Set :CFBundleName {} ({})", prefix, profile_name),
                        target_plist.to_str().unwrap_or(""),
                    ])
                    .status();
            }

            // Copy icon
            let icon_names = ["app.icns", "chrome.icns", "AppIcon.icns"];
            for icon_name in &icon_names {
                let orig_icon = orig_app.join("Contents/Resources").join(icon_name);
                if orig_icon.exists() {
                    let _ = fs::copy(&orig_icon, resources_dir.join("app.icns"));
                    break;
                }
            }
        }

        Ok(app_dir)
    }

    /// Create standalone launcher shell script for Linux.
    fn create_linux_script(
        storage: &StorageManager,
        browser: Browser,
        profile_name: &str,
        profile_dir: &Path,
        dest_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        let shortcuts_dir = match dest_dir {
            Some(p) => p.to_path_buf(),
            None => storage.base_dir().join("shortcuts"),
        };
        fs::create_dir_all(&shortcuts_dir)?;

        let script_file = shortcuts_dir.join(format!("{}-{}.sh", browser, profile_name));
        let binary_path = browser.binary_path()?;

        let script = format!(
            "#!/bin/sh\nexec \"{}\" --user-data-dir=\"{}\" --no-first-run \"$@\"\n",
            binary_path.display(),
            profile_dir.display()
        );

        fs::write(&script_file, script)?;
        let mut perms = fs::metadata(&script_file)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_file, perms)?;

        Ok(script_file)
    }
}
