mod browser;
mod cli;
mod error;
mod open;
mod policy;
mod proxy;
mod shortcut;
mod storage;

use std::io::{self, Write};
use clap::Parser;
use colored::Colorize;

use crate::cli::{Cli, Commands, ProxyAction};
use crate::error::Result;
use crate::open::OpenLauncher;
use crate::proxy::ProxyConfig;
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
                "  {:<3} {:<18} {:<32} {:<24} {}",
                "ACT",
                "NAME".bold(),
                "DESCRIPTION".bold(),
                "PROXY".bold(),
                "CREATED".bold()
            );
            println!("  {}", "-".repeat(96).dimmed());

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

                let proxy_display = match &meta.proxy {
                    Some(p) => truncate(&p.to_string(), 22).cyan().to_string(),
                    None => "-".dimmed().to_string(),
                };

                let created_str = meta.created_at.format("%Y-%m-%d %H:%M").to_string();

                println!(
                    "  {:<3} {:<18} {:<32} {:<24} {}",
                    act_marker,
                    name_display,
                    desc_display,
                    proxy_display,
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
                    if let Some((meta, _)) = storage
                        .list_profiles(browser)?
                        .into_iter()
                        .find(|(m, _)| m.name == name)
                    {
                        match &meta.proxy {
                            Some(p) => println!("Egress proxy: {}", p.to_string().cyan().bold()),
                            None => println!("Egress proxy: {}", "none".dimmed()),
                        }
                    }
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

            // Global-launch proxy follows the active profile (managed policy
            // on macOS, .desktop override on Linux).
            policy::sync_for_active_profile(&storage, browser)?;
            if let Some((meta, _)) = storage
                .list_profiles(browser)?
                .into_iter()
                .find(|(m, _)| m.name == name)
            {
                if let Some(p) = &meta.proxy {
                    println!(
                        "{} Egress proxy active for global launches — effective on the next browser start.",
                        "Note:".blue().bold()
                    );
                    warn_side_by_side_override(&storage, browser, p);
                }
            }
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

        Commands::Open { name, urls } => {
            OpenLauncher::launch(&storage, browser, &name, &urls)?;
            println!(
                "{} Launched {} profile '{}' — global default profile unchanged.",
                "Success:".green().bold(),
                browser.display_name().cyan(),
                name.bold()
            );
        }

        Commands::Proxy { action } => match action {
            ProxyAction::Set { name, server, bypass } => {
                let cfg = ProxyConfig::parse(&server, bypass.as_deref())?;
                let meta = storage.set_proxy(&name, browser, Some(cfg))?;

                println!(
                    "{} Proxy for profile '{}' set to {}.",
                    "Success:".green().bold(),
                    meta.name.bold(),
                    meta.proxy.as_ref().unwrap().to_string().cyan().bold()
                );
                let is_active = is_active_profile(&storage, browser, &meta.name);
                if is_active {
                    policy::sync_for_active_profile(&storage, browser)?;
                }
                print_proxy_followup(&meta.name, is_active);
            }

            ProxyAction::Show { name } => {
                let profiles = storage.list_profiles(browser)?;
                match name {
                    Some(name) => {
                        let (meta, is_active) = profiles.into_iter().find(|(m, _)| m.name == name)
                            .ok_or_else(|| crate::error::BmError::ProfileNotFound {
                                name: name.clone(),
                                browser: browser.display_name().to_string(),
                            })?;
                        println!(
                            "Proxy for {} profile '{}'{}:",
                            browser.display_name().cyan(),
                            if is_active { meta.name.green().bold().to_string() } else { meta.name.bold().to_string() },
                            if is_active { " (active)" } else { "" }
                        );
                        match &meta.proxy {
                            Some(p) => {
                                println!("  Server: {}", p.to_string().cyan().bold());
                                match p.bypass_string() {
                                    Some(b) => println!("  Bypass: {}", b),
                                    None => println!("  Bypass: {}", "-".dimmed()),
                                }
                            }
                            None => println!("  {}", "No proxy configured.".dimmed()),
                        }
                    }
                    None => {
                        println!(
                            "{} for {}:",
                            "Proxy Configuration".bold(),
                            browser.display_name().cyan().bold()
                        );
                        println!("  {:<18} {:<28} {}", "NAME".bold(), "SERVER".bold(), "BYPASS".bold());
                        println!("  {}", "-".repeat(64).dimmed());
                        for (meta, is_active) in profiles {
                            let (server, bypass) = match &meta.proxy {
                                Some(p) => (
                                    truncate(&p.to_string(), 26),
                                    p.bypass_string().unwrap_or_else(|| "-".into()),
                                ),
                                None => ("-".dimmed().to_string(), "-".dimmed().to_string()),
                            };
                            let name_display = if is_active {
                                format!("{} {}", meta.name, "*".green().bold())
                            } else {
                                meta.name.clone()
                            };
                            println!("  {:<18} {:<28} {}", name_display, server, bypass);
                        }
                    }
                }
            }

            ProxyAction::Clear { name } => {
                let meta = storage.set_proxy(&name, browser, None)?;
                println!(
                    "{} Proxy removed from profile '{}'.",
                    "Success:".green().bold(),
                    meta.name.bold()
                );
                let is_active = is_active_profile(&storage, browser, &meta.name);
                if is_active {
                    policy::sync_for_active_profile(&storage, browser)?;
                }
                print_proxy_followup(&meta.name, is_active);
            }
        },
    }

    Ok(())
}

/// Truncate a string to `max` characters (char-boundary safe) with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(2)).collect();
        format!("{}...", kept)
    }
}

/// Shared hints after `bm proxy set/clear`.
fn print_proxy_followup(name: &str, is_active: bool) {
    if is_active {
        println!(
            "{} '{}' is the active profile — global-launch proxy policy updated (restart the browser to apply).",
            "Note:".blue().bold(),
            name.yellow().bold()
        );
    }
    println!(
        "Existing standalone shortcuts for '{}' were generated with the previous flags — regenerate them with {} to apply the proxy there too.",
        name.bold(),
        format!("bm shortcut {}", name).green()
    );
}

fn is_active_profile(storage: &StorageManager, browser: crate::browser::Browser, name: &str) -> bool {
    storage
        .get_current_active(browser)
        .ok()
        .flatten()
        .as_deref()
        == Some(name)
}

/// On macOS the active profile's managed policy overrides the proxy flags of
/// side-by-side shortcut launches of the same browser (policy > command line
/// in Chromium). Warn when another profile also carries a proxy, since those
/// shortcuts would silently ride the active profile's egress.
fn warn_side_by_side_override(
    storage: &StorageManager,
    browser: crate::browser::Browser,
    active_proxy: &ProxyConfig,
) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let conflicting = storage
        .list_profiles(browser)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, is_active)| !*is_active)
        .filter_map(|(m, _)| m.proxy)
        .any(|p| p != *active_proxy);
    if conflicting {
        println!(
            "{} Other profiles also carry a proxy. While this profile is active, macOS managed policy overrides their shortcut proxies — side-by-side launches will use {} instead. Activate the other profile (or clear this proxy) before using them.",
            "Warning:".yellow().bold(),
            active_proxy.to_string().yellow()
        );
    }
}
