mod browser;
mod cli;
mod error;
mod shortcut;
mod storage;

use std::io::{self, Write};
use clap::Parser;
use colored::Colorize;

use crate::cli::{Cli, Commands};
use crate::error::Result;
use crate::shortcut::ShortcutGenerator;
use crate::storage::StorageManager;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{} {}", "Error:".red().bold(), err);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let browser = cli.browser;
    let storage = StorageManager::new()?;

    match cli.command {
        Commands::List => {
            let profiles = storage.list_profiles(browser)?;
            let active_opt = storage.get_current_active(browser)?;

            println!(
                "{} for {}:",
                "Managed Profiles".bold(),
                browser.display_name().cyan().bold()
            );

            if profiles.is_empty() {
                println!(
                    "  {} No profiles found. Create one with '{}'.",
                    "-".dimmed(),
                    format!("bm create <name> --browser {}", browser).green()
                );
                if let Some(active) = active_opt {
                    println!("  Current active status: {}", active.yellow());
                }
                return Ok(());
            }

            println!(
                "  {:<3} {:<18} {:<32} {}",
                "ACT",
                "NAME".bold(),
                "DESCRIPTION".bold(),
                "CREATED".bold()
            );
            println!("  {}", "-".repeat(72).dimmed());

            for (meta, is_active) in profiles {
                let act_marker = if is_active {
                    "*".green().bold().to_string()
                } else {
                    " ".to_string()
                };

                let name_display = if is_active {
                    meta.name.green().bold().to_string()
                } else {
                    meta.name.to_string()
                };

                let desc_display = if meta.description.is_empty() {
                    "-".dimmed().to_string()
                } else if meta.description.len() > 30 {
                    format!("{}...", &meta.description[..27])
                } else {
                    meta.description.clone()
                };

                let created_str = meta.created_at.format("%Y-%m-%d %H:%M").to_string();

                println!(
                    "  {:<3} {:<18} {:<32} {}",
                    act_marker,
                    name_display,
                    desc_display,
                    created_str.dimmed()
                );
            }
        }

        Commands::Current => {
            let active = storage.get_current_active(browser)?;
            match active {
                Some(name) => {
                    println!(
                        "Currently active profile for {}: {}",
                        browser.display_name().cyan(),
                        name.green().bold()
                    );
                }
                None => {
                    println!(
                        "Currently active profile for {}: {} (default unmanaged data)",
                        browser.display_name().cyan(),
                        "none".yellow()
                    );
                }
            }
        }

        Commands::Create { name, note } => {
            let meta = storage.create_profile(&name, note.as_deref(), browser)?;
            println!(
                "{} Created profile '{}' for {}.",
                "Success:".green().bold(),
                meta.name.bold(),
                browser.display_name().cyan()
            );
            println!(
                "To activate it, run: {}",
                format!("bm switch {} --browser {}", meta.name, browser).green()
            );
        }

        Commands::Switch { name, force } => {
            println!(
                "Switching {} profile to '{}'...",
                browser.display_name().cyan(),
                name.bold()
            );

            let backup = storage.switch_profile(&name, browser, force)?;

            if let Some(backup_name) = backup {
                println!(
                    "{} Your pre-existing browser data has been safely preserved as profile '{}'.",
                    "Note:".blue().bold(),
                    backup_name.yellow().bold()
                );
            }

            println!(
                "{} Successfully switched active profile to '{}'!",
                "Success:".green().bold(),
                name.green().bold()
            );
            println!("All default browser launches (Dock, links, Spotlight) will now use this profile.");
        }

        Commands::Describe { name, note } => {
            let meta = storage.describe_profile(&name, &note, browser)?;
            println!(
                "{} Updated description for profile '{}': \"{}\"",
                "Success:".green().bold(),
                meta.name.bold(),
                meta.description
            );
        }

        Commands::Delete { name, yes } => {
            if !yes {
                print!(
                    "Are you sure you want to permanently delete profile '{}' and all its user data? [y/N]: ",
                    name.red().bold()
                );
                io::stdout().flush().map_err(crate::error::BmError::Io)?;

                let mut input = String::new();
                io::stdin().read_line(&mut input).map_err(crate::error::BmError::Io)?;
                let trimmed = input.trim().to_lowercase();
                if trimmed != "y" && trimmed != "yes" {
                    println!("Deletion cancelled.");
                    return Ok(());
                }
            }

            storage.delete_profile(&name, browser)?;
            println!(
                "{} Profile '{}' has been deleted.",
                "Success:".green().bold(),
                name.bold()
            );
        }

        Commands::Shortcut { name, dest } => {
            let shortcut_path =
                ShortcutGenerator::create_shortcut(&storage, browser, &name, dest.as_deref())?;

            println!(
                "{} Standalone shortcut created at:",
                "Success:".green().bold()
            );
            println!("  {}", shortcut_path.display().to_string().cyan().bold());
            println!("You can run this launcher directly to open profile '{}' independently.", name.bold());
        }
    }

    Ok(())
}
