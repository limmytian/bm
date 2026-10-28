use thiserror::Error;

#[derive(Error, Debug)]
pub enum BmError {
    #[error("Profile '{name}' already exists for {browser}.")]
    ProfileAlreadyExists { name: String, browser: String },

    #[error("Profile '{name}' not found for {browser}.")]
    ProfileNotFound { name: String, browser: String },

    #[error("Template '{name}' already exists.")]
    TemplateAlreadyExists { name: String },

    #[error("Template '{name}' not found.")]
    TemplateNotFound { name: String },

    #[error("Group '{name}' already exists.")]
    GroupAlreadyExists { name: String },

    #[error("Group '{name}' not found.")]
    GroupNotFound { name: String },

    #[error("Profile '{profile}' for {browser} is not a member of group '{group}'.")]
    GroupMemberNotFound {
        group: String,
        profile: String,
        browser: String,
    },

    #[error("Profile '{profile}' for {browser} is locked by agent lease (holder: '{holder}', expires in {remaining_secs}s). Use --force to override or release the lease first.")]
    ProfileLeased {
        profile: String,
        browser: String,
        holder: String,
        remaining_secs: i64,
    },

    #[error("No active lease found for profile '{profile}' on {browser}.")]
    LeaseNotFound { profile: String, browser: String },

    #[error("Cannot delete active profile '{name}'. Switch to another profile first.")]
    ActiveProfileCannotBeDeleted { name: String },

    #[error("Invalid profile name '{0}'. Names must contain only alphanumeric characters, dashes, and underscores (1-64 characters).")]
    InvalidProfileName(String),

    #[error("Browser '{browser}' is currently running. Please close it before switching profiles (or use --force).")]
    BrowserRunning { browser: String },

    #[error("Invalid proxy configuration: {0}")]
    InvalidProxy(String),

    #[error("metadata.json uses schema version {found}, but this bm build supports up to {supported}. The file was written by a newer bm — upgrade bm before touching profiles (nothing was modified).")]
    MetadataVersionTooNew { found: u32, supported: u32 },

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
