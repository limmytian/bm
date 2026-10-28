use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

use crate::error::{BmError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    #[value(name = "chrome", alias = "google-chrome")]
    Chrome,
    #[value(name = "edge", alias = "microsoft-edge")]
    Edge,
    #[value(name = "brave", alias = "brave-browser")]
    Brave,
    #[value(name = "chromium", alias = "chromium-browser")]
    Chromium,
}

impl fmt::Display for Browser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Browser::Chrome => write!(f, "chrome"),
            Browser::Edge => write!(f, "edge"),
            Browser::Brave => write!(f, "brave"),
            Browser::Chromium => write!(f, "chromium"),
        }
    }
}

impl Browser {
    /// Return the human-readable display name of the browser.
    pub fn display_name(&self) -> &'static str {
        match self {
            Browser::Chrome => "Google Chrome",
            Browser::Edge => "Microsoft Edge",
            Browser::Brave => "Brave",
            Browser::Chromium => "Chromium",
        }
    }

    /// macOS managed-policy domain (`defaults`) of this browser.
    pub fn policy_domain(&self) -> &'static str {
        match self {
            Browser::Chrome => "com.google.Chrome",
            Browser::Edge => "com.microsoft.Edge",
            Browser::Brave => "com.brave.Browser",
            Browser::Chromium => "org.chromium.Chromium",
        }
    }

    /// Application bundle name on macOS (e.g. "Google Chrome.app").
    pub fn app_bundle_name(&self) -> &'static str {
        match self {
            Browser::Chrome => "Google Chrome.app",
            Browser::Edge => "Microsoft Edge.app",
            Browser::Brave => "Brave Browser.app",
            Browser::Chromium => "Chromium.app",
        }
    }

    /// Relative executable binary path inside a macOS application bundle.
    pub fn macos_relative_binary(&self) -> &'static str {
        match self {
            Browser::Chrome => "Contents/MacOS/Google Chrome",
            Browser::Edge => "Contents/MacOS/Microsoft Edge",
            Browser::Brave => "Contents/MacOS/Brave Browser",
            Browser::Chromium => "Contents/MacOS/Chromium",
        }
    }

    /// Bundle identifiers for macOS Spotlight (`mdfind`) auto-discovery.
    pub fn macos_bundle_ids(&self) -> &'static [&'static str] {
        match self {
            Browser::Chrome => &["com.google.Chrome", "com.google.Chrome.canary"],
            Browser::Edge => &[
                "com.microsoft.edgemac",
                "com.microsoft.Edge",
                "com.microsoft.edgemac.Canary",
                "com.microsoft.edgemac.Dev",
            ],
            Browser::Brave => &[
                "com.brave.Browser",
                "com.brave.Browser.beta",
                "com.brave.Browser.nightly",
            ],
            Browser::Chromium => &["org.chromium.Chromium"],
        }
    }

    /// Linux candidate executable names.
    pub fn linux_binary_names(&self) -> &'static [&'static str] {
        match self {
            Browser::Chrome => &["google-chrome", "google-chrome-stable"],
            Browser::Edge => &["microsoft-edge", "microsoft-edge-stable"],
            Browser::Brave => &["brave-browser", "brave"],
            Browser::Chromium => &["chromium", "chromium-browser"],
        }
    }

    /// Environment variable names that override the binary/bundle path for this browser,
    /// in order of precedence: dedicated browser var first, then global var.
    pub fn env_var_names(&self) -> Vec<&'static str> {
        match self {
            Browser::Chrome => vec![
                "BM_CHROME_PATH",
                "BM_CHROME_BIN",
                "BM_BROWSER_PATH",
                "BM_BROWSER_BIN",
            ],
            Browser::Edge => vec![
                "BM_EDGE_PATH",
                "BM_EDGE_BIN",
                "BM_BROWSER_PATH",
                "BM_BROWSER_BIN",
            ],
            Browser::Brave => vec![
                "BM_BRAVE_PATH",
                "BM_BRAVE_BIN",
                "BM_BROWSER_PATH",
                "BM_BROWSER_BIN",
            ],
            Browser::Chromium => vec![
                "BM_CHROMIUM_PATH",
                "BM_CHROMIUM_BIN",
                "BM_BROWSER_PATH",
                "BM_BROWSER_BIN",
            ],
        }
    }

    /// XDG desktop entry id of this browser on Linux.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn linux_desktop_file(&self) -> &'static str {
        match self {
            Browser::Chrome => "google-chrome.desktop",
            Browser::Edge => "microsoft-edge.desktop",
            Browser::Brave => "brave-browser.desktop",
            Browser::Chromium => "chromium.desktop",
        }
    }

    /// Return the default system user data directory for this browser.
    pub fn default_data_dir(&self) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| {
            BmError::Other("Unable to resolve current user's home directory".into())
        })?;

        if cfg!(target_os = "macos") {
            match self {
                Browser::Chrome => Ok(home.join("Library/Application Support/Google/Chrome")),
                Browser::Edge => Ok(home.join("Library/Application Support/Microsoft Edge")),
                Browser::Brave => {
                    Ok(home.join("Library/Application Support/BraveSoftware/Brave-Browser"))
                }
                Browser::Chromium => Ok(home.join("Library/Application Support/Chromium")),
            }
        } else if cfg!(target_os = "linux") {
            match self {
                Browser::Chrome => Ok(home.join(".config/google-chrome")),
                Browser::Edge => Ok(home.join(".config/microsoft-edge")),
                Browser::Brave => Ok(home.join(".config/BraveSoftware/Brave-Browser")),
                Browser::Chromium => Ok(home.join(".config/chromium")),
            }
        } else {
            Err(BmError::UnsupportedPlatform(
                "Only macOS and Linux are supported at this moment.".into(),
            ))
        }
    }

    /// Resolve the application bundle path and its discovery source on macOS.
    pub fn resolve_app_path(&self) -> Result<(PathBuf, ResolutionSource)> {
        if cfg!(target_os = "macos") {
            // 1. Check environment variables
            for var in self.env_var_names() {
                if let Ok(val) = std::env::var(var) {
                    let val = val.trim();
                    if !val.is_empty() {
                        let path = PathBuf::from(val);
                        if path.exists() {
                            if path.is_dir()
                                && path.extension().and_then(|s| s.to_str()) == Some("app")
                            {
                                return Ok((path, ResolutionSource::Env(var.to_string())));
                            }
                            // If path points inside a .app, locate enclosing .app
                            for ancestor in path.ancestors() {
                                if ancestor.is_dir()
                                    && ancestor.extension().and_then(|s| s.to_str()) == Some("app")
                                {
                                    return Ok((
                                        ancestor.to_path_buf(),
                                        ResolutionSource::Env(var.to_string()),
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            let bundle_name = self.app_bundle_name();

            // 2. System /Applications
            let sys_app = PathBuf::from("/Applications").join(bundle_name);
            if sys_app.exists() {
                return Ok((sys_app, ResolutionSource::StandardSystem));
            }

            // 3. User ~/Applications
            if let Some(home) = dirs::home_dir() {
                let user_app = home.join("Applications").join(bundle_name);
                if user_app.exists() {
                    return Ok((user_app, ResolutionSource::StandardUser));
                }
            }

            // 4. Spotlight / mdfind by bundle ID
            if let Some(found) = mdfind_bundle(self.macos_bundle_ids()) {
                return Ok((found, ResolutionSource::Spotlight));
            }

            Err(BmError::Other(format!(
                "{} application bundle ({}) was not found (checked env, /Applications, ~/Applications, and Spotlight)",
                self.display_name(),
                bundle_name
            )))
        } else {
            Err(BmError::UnsupportedPlatform(
                "App bundle resolution is only applicable on macOS".into(),
            ))
        }
    }

    /// Return the executable application path on macOS.
    pub fn app_path(&self) -> Result<PathBuf> {
        self.resolve_app_path().map(|(p, _)| p)
    }

    /// Resolve the main executable binary path and its discovery source.
    pub fn resolve_binary_path(&self) -> Result<(PathBuf, ResolutionSource)> {
        if cfg!(target_os = "macos") {
            // 1. Check environment variables first
            for var in self.env_var_names() {
                if let Ok(val) = std::env::var(var) {
                    let val = val.trim();
                    if !val.is_empty() {
                        let path = PathBuf::from(val);
                        if path.exists() {
                            if path.is_file() {
                                return Ok((path, ResolutionSource::Env(var.to_string())));
                            }
                            if path.is_dir() {
                                let rel = self.macos_relative_binary();
                                let candidate = path.join(rel);
                                if candidate.exists() {
                                    return Ok((candidate, ResolutionSource::Env(var.to_string())));
                                }
                                let macos_dir = path.join("Contents/MacOS");
                                if let Ok(entries) = std::fs::read_dir(&macos_dir) {
                                    for entry in entries.flatten() {
                                        let p = entry.path();
                                        if p.is_file() {
                                            return Ok((p, ResolutionSource::Env(var.to_string())));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 2. Resolve via app bundle
            let (app, source) = self.resolve_app_path()?;
            let binary = app.join(self.macos_relative_binary());
            if binary.exists() {
                Ok((binary, source))
            } else {
                let macos_dir = app.join("Contents/MacOS");
                if let Ok(entries) = std::fs::read_dir(&macos_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() {
                            return Ok((p, source));
                        }
                    }
                }
                Err(BmError::Other(format!(
                    "Binary not found inside application bundle: {}",
                    binary.display()
                )))
            }
        } else if cfg!(target_os = "linux") {
            // 1. Check environment variables
            for var in self.env_var_names() {
                if let Ok(val) = std::env::var(var) {
                    let val = val.trim();
                    if !val.is_empty() {
                        let path = PathBuf::from(val);
                        if path.exists() && path.is_file() {
                            return Ok((path, ResolutionSource::Env(var.to_string())));
                        }
                    }
                }
            }

            let names = self.linux_binary_names();

            // 2. Check PATH
            if let Some(path_os) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_os) {
                    for name in names {
                        let candidate = dir.join(name);
                        if candidate.is_file() {
                            return Ok((candidate, ResolutionSource::PathEnv));
                        }
                    }
                }
            }

            // 3. Fallback well-known paths
            let common_dirs = [
                "/snap/bin",
                "/usr/bin",
                "/usr/local/bin",
                "/opt/google/chrome",
                "/opt/microsoft/msedge",
            ];
            for dir in &common_dirs {
                let base = PathBuf::from(dir);
                for name in names {
                    let candidate = base.join(name);
                    if candidate.is_file() {
                        return Ok((
                            candidate.clone(),
                            ResolutionSource::SystemFallback(candidate.display().to_string()),
                        ));
                    }
                }
            }

            // 4. Default command name fallback
            Ok((PathBuf::from(names[0]), ResolutionSource::PathEnv))
        } else {
            Err(BmError::UnsupportedPlatform(
                "Only macOS and Linux are supported.".into(),
            ))
        }
    }

    /// Return the main executable binary path.
    pub fn binary_path(&self) -> Result<PathBuf> {
        self.resolve_binary_path().map(|(p, _)| p)
    }

    /// Check if there is an active running process of this browser.
    pub fn is_running(&self) -> bool {
        let process_name = match self {
            Browser::Chrome => {
                if cfg!(target_os = "macos") {
                    "Google Chrome"
                } else {
                    "chrome"
                }
            }
            Browser::Edge => {
                if cfg!(target_os = "macos") {
                    "Microsoft Edge"
                } else {
                    "msedge"
                }
            }
            Browser::Brave => {
                if cfg!(target_os = "macos") {
                    "Brave Browser"
                } else {
                    "brave"
                }
            }
            Browser::Chromium => {
                if cfg!(target_os = "macos") {
                    "Chromium"
                } else {
                    "chromium"
                }
            }
        };

        let output = Command::new("pgrep").arg("-f").arg(process_name).output();

        match output {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }

    /// Gracefully or forcefully terminate running browser processes.
    pub fn terminate(&self) -> Result<()> {
        let process_name = match self {
            Browser::Chrome => {
                if cfg!(target_os = "macos") {
                    "Google Chrome"
                } else {
                    "chrome"
                }
            }
            Browser::Edge => {
                if cfg!(target_os = "macos") {
                    "Microsoft Edge"
                } else {
                    "msedge"
                }
            }
            Browser::Brave => {
                if cfg!(target_os = "macos") {
                    "Brave Browser"
                } else {
                    "brave"
                }
            }
            Browser::Chromium => {
                if cfg!(target_os = "macos") {
                    "Chromium"
                } else {
                    "chromium"
                }
            }
        };

        // Try pkill -f first (standard across macOS and modern Linux)
        let status = Command::new("pkill").arg("-f").arg(process_name).status();

        match status {
            Ok(_) => Ok(()),
            Err(_) => {
                // Fallback to killall if pkill is absent
                let _ = Command::new("killall").arg(process_name).status();
                Ok(())
            }
        }
    }
}

/// Where a browser application bundle or binary was discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    /// Explicit environment variable override (e.g. `BM_CHROME_PATH`, `BM_BROWSER_PATH`).
    Env(String),
    /// Standard system application directory (`/Applications`).
    StandardSystem,
    /// User-level application directory (`~/Applications`).
    StandardUser,
    /// Discovered via macOS Spotlight (`mdfind`) by bundle identifier.
    Spotlight,
    /// Discovered via system `PATH`.
    PathEnv,
    /// System fallback location (e.g. `/snap/bin/...`).
    SystemFallback(String),
}

impl fmt::Display for ResolutionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolutionSource::Env(var) => write!(f, "env:{}", var),
            ResolutionSource::StandardSystem => write!(f, "system"),
            ResolutionSource::StandardUser => write!(f, "user"),
            ResolutionSource::Spotlight => write!(f, "spotlight"),
            ResolutionSource::PathEnv => write!(f, "path"),
            ResolutionSource::SystemFallback(loc) => write!(f, "fallback:{}", loc),
        }
    }
}

#[cfg(target_os = "macos")]
fn mdfind_bundle(bundle_ids: &[&str]) -> Option<PathBuf> {
    for id in bundle_ids {
        let query = format!("kMDItemCFBundleIdentifier == '{}'", id);
        if let Ok(output) = Command::new("mdfind").arg(&query).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let line = line.trim();
                    if line.is_empty()
                        || line.contains("/.Trash/")
                        || line.contains("/Trash/")
                        || line.contains("EdgeUpdater")
                        || line.contains("Sparkle")
                    {
                        continue;
                    }
                    let path = PathBuf::from(line);
                    if path.is_dir() && path.extension().and_then(|s| s.to_str()) == Some("app") {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn mdfind_bundle(_bundle_ids: &[&str]) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn env_var_precedence_and_independent_resolution() {
        let tmp = tempdir().unwrap();
        let chrome_bin = tmp.path().join("fake-chrome");
        let edge_bin = tmp.path().join("fake-edge");
        let fallback_bin = tmp.path().join("fake-fallback");
        std::fs::write(&chrome_bin, b"#!/bin/sh\n").unwrap();
        std::fs::write(&edge_bin, b"#!/bin/sh\n").unwrap();
        std::fs::write(&fallback_bin, b"#!/bin/sh\n").unwrap();

        // 1. Independent per-browser env vars
        std::env::set_var("BM_CHROME_BIN", &chrome_bin);
        std::env::set_var("BM_EDGE_PATH", &edge_bin);

        let (res_chrome, src_chrome) = Browser::Chrome.resolve_binary_path().unwrap();
        assert_eq!(res_chrome, chrome_bin);
        assert_eq!(
            src_chrome,
            ResolutionSource::Env("BM_CHROME_BIN".to_string())
        );

        let (res_edge, src_edge) = Browser::Edge.resolve_binary_path().unwrap();
        assert_eq!(res_edge, edge_bin);
        assert_eq!(src_edge, ResolutionSource::Env("BM_EDGE_PATH".to_string()));

        // Clean up per-browser vars
        std::env::remove_var("BM_CHROME_BIN");
        std::env::remove_var("BM_EDGE_PATH");

        // 2. Global fallback env var
        std::env::set_var("BM_BROWSER_PATH", &fallback_bin);
        let (res_fallback, src_fallback) = Browser::Brave.resolve_binary_path().unwrap();
        assert_eq!(res_fallback, fallback_bin);
        assert_eq!(
            src_fallback,
            ResolutionSource::Env("BM_BROWSER_PATH".to_string())
        );

        std::env::remove_var("BM_BROWSER_PATH");
    }

    #[test]
    fn browser_properties_and_candidates() {
        assert_eq!(Browser::Chrome.app_bundle_name(), "Google Chrome.app");
        assert_eq!(Browser::Edge.app_bundle_name(), "Microsoft Edge.app");
        assert_eq!(Browser::Brave.app_bundle_name(), "Brave Browser.app");
        assert_eq!(Browser::Chromium.app_bundle_name(), "Chromium.app");

        assert!(Browser::Chrome
            .macos_bundle_ids()
            .contains(&"com.google.Chrome"));
        assert!(Browser::Edge
            .macos_bundle_ids()
            .contains(&"com.microsoft.edgemac"));
        assert!(Browser::Brave
            .macos_bundle_ids()
            .contains(&"com.brave.Browser"));
        assert!(Browser::Chromium
            .macos_bundle_ids()
            .contains(&"org.chromium.Chromium"));

        assert!(Browser::Chrome
            .linux_binary_names()
            .contains(&"google-chrome"));
        assert!(Browser::Edge
            .linux_binary_names()
            .contains(&"microsoft-edge"));
        assert!(Browser::Brave
            .linux_binary_names()
            .contains(&"brave-browser"));
        assert!(Browser::Chromium.linux_binary_names().contains(&"chromium"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_app_bundle_resolution_with_env_mock() {
        let tmp = tempdir().unwrap();
        let app_dir = tmp.path().join("MockBrowser.app");
        let macos_dir = app_dir.join("Contents/MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        let bin_file = macos_dir.join("MockBrowser");
        std::fs::write(&bin_file, b"#!/bin/sh\n").unwrap();

        std::env::set_var("BM_CHROMIUM_PATH", &app_dir);
        let (app, src) = Browser::Chromium.resolve_app_path().unwrap();
        assert_eq!(app, app_dir);
        assert_eq!(src, ResolutionSource::Env("BM_CHROMIUM_PATH".to_string()));

        let (bin, bin_src) = Browser::Chromium.resolve_binary_path().unwrap();
        assert_eq!(bin, bin_file);
        assert_eq!(
            bin_src,
            ResolutionSource::Env("BM_CHROMIUM_PATH".to_string())
        );

        std::env::remove_var("BM_CHROMIUM_PATH");
    }
}
