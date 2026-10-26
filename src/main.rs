mod browser;
mod cache;
mod cli;
mod error;
mod open;
mod policy;
mod proxy;
mod proxy_test;
mod shortcut;
mod size;
mod storage;

use std::io::{self, Write};
use clap::Parser;
use colored::Colorize;

use crate::cli::{Cli, Commands, ProxyAction};
use crate::error::Result;
use crate::open::OpenLauncher;
use crate::proxy::ProxyConfig;
use crate::shortcut::ShortcutGenerator;
use crate::size::{format_bytes, measure, purge_caches, scan_regenerable};
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

        Commands::Clone { src, dst, full, no_proxy } => {
            let (meta, copied) = storage.clone_profile(&src, &dst, browser, full, !no_proxy)?;
            println!(
                "{} Cloned profile '{}' → '{}' ({} files).",
                "Success:".green().bold(),
                src.bold(),
                meta.name.bold(),
                copied
            );
            if !full {
                println!(
                    "Regenerable caches were skipped for a fast copy — use {} for a byte-exact one.",
                    "--full".green()
                );
            }
            if no_proxy {
                println!("{} Egress proxy not carried over ({}).", "Note:".blue().bold(), "--no-proxy".green());
            }
            println!(
                "To activate the clone, run: {}",
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

        Commands::Rename { old, new, regen_shortcuts } => {
            let meta = storage.rename_profile(&old, &new, browser)?;
            println!(
                "{} Renamed profile '{}' → '{}' (data and metadata moved).",
                "Success:".green().bold(),
                old.bold(),
                meta.name.bold()
            );

            let stale = ShortcutGenerator::find_stale_shortcuts(&storage, browser, &old);
            if stale.is_empty() {
                return Ok(());
            }
            if regen_shortcuts {
                let regenerated =
                    ShortcutGenerator::regenerate_stale_shortcuts(&storage, browser, &stale, &new)?;
                println!(
                    "{} {} standalone shortcut(s) regenerated.",
                    "Success:".green().bold(),
                    regenerated.len()
                );
            } else {
                println!(
                    "{} {} standalone shortcut(s) still reference the old name:",
                    "Warning:".yellow().bold(),
                    stale.len()
                );
                for path in &stale {
                    println!("  {}", path.display());
                }
                println!(
                    "Regenerate them with {} or re-run this command with {}.",
                    format!("bm shortcut {}", new).green(),
                    "--regen-shortcuts".green()
                );
            }
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

        Commands::Size { name, json } => {
            let names: Vec<String> = match &name {
                Some(name) => {
                    if !storage.profile_dir(browser, name).exists() {
                        return Err(crate::error::BmError::ProfileNotFound {
                            name: name.clone(),
                            browser: browser.display_name().to_string(),
                        });
                    }
                    vec![name.clone()]
                }
                None => storage
                    .list_profiles(browser)?
                    .into_iter()
                    .map(|(m, _)| m.name)
                    .collect(),
            };

            if json {
                let entries: Vec<serde_json::Value> = names
                    .iter()
                    .map(|n| {
                        let s = measure(&storage.profile_dir(browser, n))?;
                        Ok(serde_json::json!({
                            "name": n,
                            "total_bytes": s.total_bytes,
                            "cache_bytes": s.cache_bytes,
                            "core_bytes": s.core_bytes,
                        }))
                    })
                    .collect::<Result<_>>()?;
                println!("{}", serde_json::to_string_pretty(&entries)?);
                return Ok(());
            }

            let mut rows: Vec<(String, crate::size::ProfileSize)> = names
                .iter()
                .map(|n| Ok((n.clone(), measure(&storage.profile_dir(browser, n))?)))
                .collect::<Result<_>>()?;
            rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.total_bytes));

            if let Some(single) = &name {
                let (_, s) = &rows[0];
                println!(
                    "Profile '{}' for {}: {} total",
                    single.bold(),
                    browser.display_name().cyan(),
                    format_bytes(s.total_bytes).bold()
                );
                println!("  Core data (sessions, logins, history): {}", format_bytes(s.core_bytes));
                println!(
                    "  Regenerable cache: {} — reclaim with {}",
                    format_bytes(s.cache_bytes).yellow(),
                    format!("bm clean {}", single).green()
                );
            } else {
                println!(
                    "{} for {} (cache = regenerable, core = valuable):",
                    "Disk Usage".bold(),
                    browser.display_name().cyan().bold()
                );
                if rows.is_empty() {
                    println!("  {} No profiles found.", "-".dimmed());
                    return Ok(());
                }
                println!(
                    "  {:<18} {:>10} {:>10} {:>10}",
                    "NAME".bold(),
                    "TOTAL".bold(),
                    "CACHE".bold(),
                    "CORE".bold()
                );
                println!("  {}", "-".repeat(52).dimmed());
                for (name, s) in rows {
                    println!(
                        "  {:<18} {:>10} {:>10} {:>10}",
                        name,
                        format_bytes(s.total_bytes),
                        format_bytes(s.cache_bytes),
                        format_bytes(s.core_bytes)
                    );
                }
            }
        }

        Commands::Clean { name, all, dry_run } => {
            if browser.is_running() {
                return Err(crate::error::BmError::BrowserRunning {
                    browser: browser.display_name().to_string(),
                });
            }
            if !all && name.is_none() {
                return Err(crate::error::BmError::Other(
                    "Specify a profile to clean, or use --all for every profile of this browser.".into(),
                ));
            }

            let targets: Vec<String> = if all {
                storage
                    .list_profiles(browser)?
                    .into_iter()
                    .map(|(m, _)| m.name)
                    .collect()
            } else {
                let n = name.as_ref().unwrap();
                if !storage.profile_dir(browser, n).exists() {
                    return Err(crate::error::BmError::ProfileNotFound {
                        name: n.clone(),
                        browser: browser.display_name().to_string(),
                    });
                }
                vec![n.clone()]
            };
            if targets.is_empty() {
                println!("No profiles found for {}.", browser.display_name().cyan());
                return Ok(());
            }

            let mut total = 0u64;
            for profile in targets {
                let dir = storage.profile_dir(browser, &profile);
                if dry_run {
                    let found = scan_regenerable(&dir)?;
                    let bytes: u64 = found.iter().map(|(_, b)| b).sum();
                    println!(
                        "{} '{}' — {} reclaimable:",
                        "Would clean".cyan().bold(),
                        profile.bold(),
                        format_bytes(bytes)
                    );
                    for (rel, b) in &found {
                        println!("    {} ({})", rel.display(), format_bytes(*b));
                    }
                    total += bytes;
                } else {
                    let reclaimed = purge_caches(&dir)?;
                    println!(
                        "{} '{}' — reclaimed {}.",
                        "Cleaned".green().bold(),
                        profile.bold(),
                        format_bytes(reclaimed)
                    );
                    total += reclaimed;
                }
            }

            if dry_run {
                println!(
                    "{} Dry run: {} reclaimable in total. Nothing deleted — re-run without --dry-run to clean.",
                    "Note:".blue().bold(),
                    format_bytes(total).yellow()
                );
            } else {
                println!(
                    "{} Reclaimed {} in total.",
                    "Success:".green().bold(),
                    format_bytes(total).bold()
                );
            }
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

            ProxyAction::Test { name, url, timeout } => {
                use crate::proxy_test::{UreqFetcher, check_egress, resolve_targets};
                use std::time::Duration;

                let targets = resolve_targets(&storage, browser, name.as_deref())?;
                let fetcher = UreqFetcher;
                let timeout = Duration::from_secs(timeout);

                for (profile, proxy) in targets {
                    let Some(proxy) = proxy else {
                        println!(
                            "{} '{}' has no proxy configured — its egress is direct, nothing to verify.",
                            "Note:".blue().bold(),
                            profile.bold()
                        );
                        continue;
                    };

                    println!("{} — {}", profile.bold(), proxy.to_string().cyan());
                    let check = check_egress(&fetcher, &url, &proxy, timeout);

                    match &check.exit_ip {
                        Some(ip) => println!(
                            "  Exit IP (via proxy):  {} ({} ms)",
                            ip.green().bold(),
                            check.latency_ms
                        ),
                        None => println!("  Exit IP (via proxy):  {}", "unreachable".red()),
                    }
                    match &check.direct_ip {
                        Some(ip) => println!("  Direct IP (no proxy): {}", ip),
                        None => println!("  Direct IP (no proxy): {}", "unavailable".dimmed()),
                    }

                    match check.effective {
                        Some(true) => println!(
                            "{} Egress goes through the proxy.",
                            "OK:".green().bold()
                        ),
                        Some(false) => println!(
                            "{} Exit IP equals the direct IP — traffic is NOT going through the proxy.",
                            "WARN:".yellow().bold()
                        ),
                        None if !check.through_ok => {
                            println!("{} {}", "FAIL:".red().bold(), "could not reach the echo endpoint through the proxy.".red());
                            if let Some(err) = &check.through_error {
                                println!("  {}", err.yellow());
                            }
                        }
                        None => println!(
                            "{} Direct comparison unavailable; proxied path works.",
                            "Note:".blue().bold()
                        ),
                    }
                }
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
