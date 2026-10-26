use thiserror::Error;

#[derive(Error, Debug)]
pub enum BmError {
    #[error("Profile '{name}' already exists for {browser}.")]
    ProfileAlreadyExists { name: String, browser: String },

    #[error("Profile '{name}' not found for {browser}.")]
    ProfileNotFound { name: String, browser: String },

    #[error("Cannot delete active profile '{name}'. Switch to another profile first.")]
    ActiveProfileCannotBeDeleted { name: String },

    #[error("Invalid profile name '{0}'. Names must contain only alphanumeric characters, dashes, and underscores (1-64 characters).")]
    InvalidProfileName(String),

    #[error("Browser '{browser}' is currently running. Please close it before switching profiles (or use --force).")]
    BrowserRunning { browser: String },

    #[error("Invalid proxy configuration: {0}")]
    InvalidProxy(String),

    #[error("Platform unsupported: {0}")]
    UnsupportedPlatform(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialize/deserialize error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, BmError>;
