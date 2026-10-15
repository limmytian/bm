use std::path::PathBuf;
use clap::{Parser, Subcommand};
use crate::browser::Browser;

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
    pub command: Commands,

    /// Target browser (chrome or edge) [default: chrome]
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

    /// Switch the active browser profile via symlink
    Switch {
        /// Name of the profile to activate
        name: String,

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
}
