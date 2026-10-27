use crate::browser::Browser;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Action for `bm backup --schedule <ACTION>`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScheduleAction {
    /// Install the daily auto-backup unit for the selected browser
    Install,
    /// Remove the scheduled unit
    Uninstall,
    /// Show whether the schedule is installed and loaded
    Status,
}

#[derive(Parser, Debug)]
#[command(
    name = "bm",
    author = "limmytian <limmytian@gmail.com>",
    version,
    about = "Browser Profile Manager - Conda-like session & profile manager for Chrome and Edge",
    long_about = "bm is a lightweight CLI to manage, switch, and launch browser user profiles (sessions, cookies, local storage, history) in a conda-like workflow."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Target browser (chrome, edge, brave, or chromium)
    #[arg(short, long, global = true, value_enum, default_value_t = Browser::Chrome)]
    pub browser: Browser,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List all managed browser profiles
    List,

    /// Show the currently active profile
    Current,

    /// Create a new clean profile
    Create {
        /// Name of the new profile (alphanumeric, dash, underscore)
        name: String,

        /// Optional note or description for this profile
        #[arg(short, long)]
        note: Option<String>,
    },

    /// Duplicate a profile on this machine (sessions survive; no cross-machine sync)
    Clone {
        /// Source profile name
        src: String,

        /// Destination profile name
        dst: String,

        /// Copy regenerable caches too (skipped by default for a fast copy)
        #[arg(long)]
        full: bool,

        /// Do not carry over the source profile's egress proxy
        #[arg(long)]
        no_proxy: bool,
    },

    /// Switch the active browser profile via symlink
    Switch {
        /// Name of the profile to activate (omit in a TTY for the interactive picker)
        name: Option<String>,

        /// Force switch by terminating running browser instances if necessary
        #[arg(short, long)]
        force: bool,
    },

    /// Set or update the description/note for a profile
    Describe {
        /// Name of the profile
        name: String,

        /// Note or description content
        note: String,
    },

    /// Rename a profile, keeping its data and metadata
    Rename {
        /// Current profile name
        old: String,

        /// New profile name
        new: String,

        /// Regenerate standalone shortcuts that reference the old name
        #[arg(long)]
        regen_shortcuts: bool,
    },

    /// Delete a profile and its isolated data
    Delete {
        /// Name of the profile to delete
        name: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Create a standalone desktop launcher / shortcut for a profile
    Shortcut {
        /// Name of the profile
        name: String,

        /// Custom destination directory for the shortcut
        #[arg(short, long)]
        dest: Option<PathBuf>,
    },

    /// Launch a profile ad-hoc without switching the global default profile
    Open {
        /// Name of the profile to launch
        name: String,

        /// Optional URLs to open in the launched browser
        urls: Vec<String>,
    },

    /// Show per-profile disk usage (cache vs core data)
    Size {
        /// Optional profile name; omit to summarize every profile
        name: Option<String>,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Delete regenerable caches from profiles to reclaim disk space
    Clean {
        /// Profile to clean; required unless --all
        name: Option<String>,

        /// Clean every profile of the selected browser
        #[arg(long)]
        all: bool,

        /// List what would be deleted without removing anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Create a local snapshot of a profile (tar+zstd, caches excluded), or
    /// verify/restore an existing snapshot
    Backup {
        /// Profile to back up; required unless --all or a mode flag is given
        name: Option<String>,

        /// Back up every profile of the selected browser (running profiles are skipped)
        #[arg(long)]
        all: bool,

        /// Include regenerable caches (skipped by default)
        #[arg(long)]
        full: bool,

        /// Keep only the N newest backups per profile (default: keep all)
        #[arg(long)]
        keep: Option<usize>,

        /// Encrypt the archive with a passphrase (BM_BACKUP_PASSPHRASE or
        /// hidden prompt; the passphrase is never accepted on the command line)
        #[arg(long)]
        encrypt: bool,

        /// Install, remove, or inspect the daily auto-backup schedule
        #[arg(long, value_enum, conflicts_with_all = ["name", "all", "full", "keep", "encrypt", "verify"])]
        schedule: Option<ScheduleAction>,

        /// Validate a snapshot archive without extracting it
        #[arg(long, value_name = "ARCHIVE", conflicts_with_all = ["all", "full", "keep", "encrypt"])]
        verify: Option<PathBuf>,
    },

    /// Restore a profile snapshot created by bm backup
    Restore {
        /// Path to a .tar.zst archive (e.g. under ~/.bm/backups)
        file: PathBuf,

        /// Restore under a different profile name
        #[arg(long)]
        name: Option<String>,

        /// Overwrite an existing profile with the same name
        #[arg(long)]
        force: bool,
    },

    /// One-screen overview: active profile, running instances, proxies, backups
    Status,

    /// Audit profile storage health and offer safe repairs
    Doctor {
        /// Apply the safe repairs (dangling symlink removal, metadata backfill)
        #[arg(long)]
        fix: bool,
    },

    /// Generate shell completions (bash, zsh, fish, powershell, nushell)
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Manage the per-profile egress proxy
    Proxy {
        #[command(subcommand)]
        action: ProxyAction,
    },

    /// Share a profile's configuration as a small bundle (no session data)
    Export {
        /// Profile to export
        name: String,

        /// Output file (default: ~/.bm/exports/bm-export-<name>-<ts>.json)
        #[arg(short, long)]
        file: Option<PathBuf>,
    },

    /// Register a profile from an exported configuration bundle
    Import {
        /// Path to an export bundle (JSON)
        file: PathBuf,

        /// Register under a different name than the bundle's
        #[arg(long)]
        name: Option<String>,

        /// Re-register over an existing profile (existing data is kept)
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProxyAction {
    /// Set the egress proxy for a profile
    Set {
        /// Name of the profile
        name: String,

        /// Proxy server as <scheme>://<host>:<port> (scheme: socks5, http, https)
        #[arg(short, long)]
        server: String,

        /// Comma-separated hosts that bypass the proxy (e.g. "localhost,*.lan")
        #[arg(long)]
        bypass: Option<String>,
    },

    /// Show the proxy configuration of one or all profiles
    Show {
        /// Optional profile name; omit to show every profile
        name: Option<String>,
    },

    /// Remove the proxy configuration from a profile
    Clear {
        /// Name of the profile
        name: String,
    },

    /// Verify a profile's egress actually goes through its proxy
    Test {
        /// Profile to verify; omit to test every profile with a proxy
        name: Option<String>,

        /// IP-echo endpoint fetched through the proxy
        #[arg(long, default_value = "https://api.ipify.org")]
        url: String,

        /// Per-request timeout in seconds
        #[arg(long, default_value_t = 5)]
        timeout: u64,
    },
}
