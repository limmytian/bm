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

    /// Return the executable application path on macOS.
    pub fn app_path(&self) -> Result<PathBuf> {
        if cfg!(target_os = "macos") {
            let app = match self {
                Browser::Chrome => PathBuf::from("/Applications/Google Chrome.app"),
                Browser::Edge => PathBuf::from("/Applications/Microsoft Edge.app"),
                Browser::Brave => PathBuf::from("/Applications/Brave Browser.app"),
                Browser::Chromium => PathBuf::from("/Applications/Chromium.app"),
            };
            if app.exists() {
                Ok(app)
            } else {
                Err(BmError::Other(format!(
                    "{} application bundle was not found at {}",
                    self.display_name(),
                    app.display()
                )))
            }
        } else {
            Err(BmError::UnsupportedPlatform(
                "App bundle resolution is only applicable on macOS".into(),
            ))
        }
    }

    /// Return the main executable binary path.
    pub fn binary_path(&self) -> Result<PathBuf> {
        if cfg!(target_os = "macos") {
            let app = self.app_path()?;
            let binary = match self {
                Browser::Chrome => app.join("Contents/MacOS/Google Chrome"),
                Browser::Edge => app.join("Contents/MacOS/Microsoft Edge"),
                Browser::Brave => app.join("Contents/MacOS/Brave Browser"),
                Browser::Chromium => app.join("Contents/MacOS/Chromium"),
            };
            if binary.exists() {
                Ok(binary)
            } else {
                Err(BmError::Other(format!(
                    "Binary not found inside application bundle: {}",
                    binary.display()
                )))
            }
        } else if cfg!(target_os = "linux") {
            let name = match self {
                Browser::Chrome => "google-chrome",
                Browser::Edge => "microsoft-edge",
                Browser::Brave => "brave-browser",
                Browser::Chromium => "chromium",
            };
            Ok(PathBuf::from(name))
        } else {
            Err(BmError::UnsupportedPlatform(
                "Only macOS and Linux are supported.".into(),
            ))
        }
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
