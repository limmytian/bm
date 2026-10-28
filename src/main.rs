mod backup;
mod browser;
mod cache;
mod cdp;
mod cli;
mod doctor;
mod error;
mod group;
mod mcp;
mod open;
mod policy;
mod proxy;
mod proxy_test;
mod report;
mod running;
mod schedule;
mod shortcut;
mod size;
mod status;
mod storage;
mod template;
mod transfer;

use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};

use crate::browser::Browser;
use crate::cli::{
    CdpAction, Cli, Commands, GroupAction, ProxyAction, ScheduleAction, TemplateAction,
};
use crate::error::{BmError, Result};
use crate::open::OpenLauncher;
use crate::proxy::ProxyConfig;
use crate::shortcut::ShortcutGenerator;
use crate::size::{format_bytes, measure, purge_caches, scan_regenerable};
use crate::status::{collect_status, render_status, truncate};
use crate::storage::StorageManager;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{} {}", "Error:".red().bold(), err);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    use std::io::IsTerminal;

    let browser = cli.browser;
    let storage = StorageManager::new()?;
    // Interactive affordances (profile picker) only when both ends of the
    // terminal are attached; piped/CI runs keep scripted behavior.
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    match cli.command {
        None => {
            if interactive {
                if let Some(name) = pick_profile(&storage, browser)? {
                    switch_to(&storage, browser, &name, false)?;
                }
            } else {
                use clap::CommandFactory;
                Cli::command().print_help()?;
            }
        }

        Some(command) => match command {
            Commands::List => {
                if cli.json {
                    print_json(&crate::report::list_json(&storage, browser)?)?;
                    return Ok(());
                }
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
                    "  {:<3} {:<18} {:<32} {:<24} {:<12} {}",
                    "ACT",
                    "NAME".bold(),
                    "DESCRIPTION".bold(),
                    "PROXY".bold(),
                    "SWITCHED".bold(),
                    "CREATED".bold()
                );
                println!("  {}", "-".repeat(110).dimmed());

                let last_switched_at = storage.last_switched_at(browser);
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

                    let switched_display = match (is_active, last_switched_at) {
                        (true, Some(at)) => crate::storage::relative_time(at, chrono::Utc::now()),
                        _ => "-".dimmed().to_string(),
                    };

                    let created_str = meta.created_at.format("%Y-%m-%d %H:%M").to_string();

                    println!(
                        "  {:<3} {:<18} {:<32} {:<24} {:<12} {}",
                        act_marker,
                        name_display,
                        desc_display,
                        proxy_display,
                        switched_display,
                        created_str.dimmed()
                    );
                }
            }

            Commands::Current => {
                if cli.json {
                    print_json(&crate::report::current_json(&storage, browser)?)?;
                    return Ok(());
                }
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
                                Some(p) => {
                                    println!("Egress proxy: {}", p.to_string().cyan().bold())
                                }
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

            Commands::Create {
                name,
                note,
                template,
            } => {
                let (meta, tpl_opt) = storage.create_profile_with_template(
                    &name,
                    note.as_deref(),
                    browser,
                    template.as_deref(),
                )?;
                if let Some(tpl_meta) = tpl_opt {
                    println!(
                        "{} Created profile '{}' for {} seeded from template '{}' ({}).",
                        "Success:".green().bold(),
                        meta.name.bold(),
                        browser.display_name().cyan(),
                        tpl_meta.name.bold(),
                        if tpl_meta.components.is_empty() {
                            "clean".dimmed().to_string()
                        } else {
                            tpl_meta.components.join(", ")
                        }
                    );
                } else {
                    println!(
                        "{} Created profile '{}' for {}.",
                        "Success:".green().bold(),
                        meta.name.bold(),
                        browser.display_name().cyan()
                    );
                }
                println!(
                    "To activate it, run: {}",
                    format!("bm switch {} --browser {}", meta.name, browser).green()
                );
            }

            Commands::Clone {
                src,
                dst,
                full,
                no_proxy,
            } => {
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
                    println!(
                        "{} Egress proxy not carried over ({}).",
                        "Note:".blue().bold(),
                        "--no-proxy".green()
                    );
                }
                println!(
                    "To activate the clone, run: {}",
                    format!("bm switch {} --browser {}", meta.name, browser).green()
                );
            }

            Commands::Switch { name, force } => {
                let name = match name {
                Some(raw) => resolve_switch_name(&raw, storage.previous_profile(browser))?,
                None if interactive => match pick_profile(&storage, browser)? {
                    Some(picked) => picked,
                    None => return Ok(()), // cancelled
                },
                None => {
                    return Err(crate::error::BmError::Other(
                        "Profile name required — e.g. `bm switch work` (the interactive picker needs a TTY)."
                            .into(),
                    ))
                }
            };
                switch_to(&storage, browser, &name, force)?;
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

            Commands::Rename {
                old,
                new,
                regen_shortcuts,
            } => {
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
                    let regenerated = ShortcutGenerator::regenerate_stale_shortcuts(
                        &storage, browser, &stale, &new,
                    )?;
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
                    println!(
                        "\n  {} Consider {} first — snapshots keep sessions and logins.",
                        "Tip:".blue().bold(),
                        format!("bm backup {}", name).green()
                    );
                    print!("Proceed [y/N]: ");
                    io::stdout().flush().map_err(crate::error::BmError::Io)?;

                    let mut input = String::new();
                    io::stdin()
                        .read_line(&mut input)
                        .map_err(crate::error::BmError::Io)?;
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
                println!(
                    "You can run this launcher directly to open profile '{}' independently.",
                    name.bold()
                );
            }

            Commands::Open {
                name,
                rest,
                temp,
                template,
                cdp,
            } => {
                if temp {
                    let mut urls = Vec::new();
                    if let Some(n) = name {
                        urls.push(n);
                    }
                    urls.extend(rest);

                    if cdp {
                        println!(
                            "{} --cdp hands FULL control of this ephemeral sandbox — loopback only.",
                            "Warning:".yellow().bold()
                        );
                    }

                    let outcome = OpenLauncher::launch_temp(
                        &storage,
                        browser,
                        template.as_deref(),
                        &urls,
                        cdp,
                    )?;

                    println!(
                        "{} Spawned ephemeral {} sandbox '{}'.",
                        "Success:".green().bold(),
                        browser.display_name().cyan().bold(),
                        outcome.temp_id.bold()
                    );
                    println!(
                        "  Data directory: {}",
                        outcome.temp_dir.display().to_string().cyan()
                    );
                    println!("  Process PID: {}", outcome.browser_pid);
                    if let Some(tpl) = template {
                        println!("  Seeded from template: {}", tpl.bold());
                    }
                    if let Some(endpoint) = outcome.cdp_endpoint {
                        println!("  CDP endpoint: {}", endpoint.cyan().bold());
                    }
                    println!("  Auto-cleanup: directory will be purged upon browser exit.");
                    return Ok(());
                }

                let first = name.ok_or_else(|| {
                    crate::error::BmError::Other(
                        "Profile name is required unless launching with --temp".into(),
                    )
                })?;
                let (profiles, urls) = crate::open::split_open_args(first, rest);
                if cdp {
                    println!(
                        "{} --cdp hands FULL control of this browser (sessions, cookies, everything) to anyone who can reach the endpoint — loopback only.",
                        "Warning:".yellow().bold()
                    );
                }
                let outcomes = OpenLauncher::launch_all(&storage, browser, &profiles, &urls, cdp)?;
                let failures: Vec<&crate::open::OpenOutcome> =
                    outcomes.iter().filter(|o| o.error.is_some()).collect();

                if outcomes.len() == 1 && failures.is_empty() {
                    println!(
                        "{} Launched {} profile '{}' — global default profile unchanged.",
                        "Success:".green().bold(),
                        browser.display_name().cyan(),
                        outcomes[0].profile.bold()
                    );
                    if let Some(endpoint) = &outcomes[0].cdp_endpoint {
                        if outcomes[0].cdp_reused {
                            println!("CDP endpoint (already running): {}", endpoint.cyan().bold());
                        } else {
                            println!("CDP endpoint: {}", endpoint.cyan().bold());
                        }
                        println!(
                            "  Connect with Playwright: {}",
                            format!("chromium.connect_over_cdp(\"{endpoint}\")").dimmed()
                        );
                    }
                } else {
                    for outcome in &outcomes {
                        match &outcome.error {
                            None => println!(
                                "{} '{}' launched (own user-data-dir, global default unchanged).",
                                "ok".green().bold(),
                                outcome.profile.bold()
                            ),
                            Some(err) => println!(
                                "{} '{}' — {}",
                                "fail".red().bold(),
                                outcome.profile.bold(),
                                err
                            ),
                        }
                        if let Some(endpoint) = &outcome.cdp_endpoint {
                            if outcome.cdp_reused {
                                println!(
                                    "  CDP endpoint (already running): {}",
                                    endpoint.cyan().bold()
                                );
                            } else {
                                println!("  CDP endpoint: {}", endpoint.cyan().bold());
                            }
                            println!(
                                "    Connect with Playwright: {}",
                                format!("chromium.connect_over_cdp(\"{endpoint}\")").dimmed()
                            );
                        }
                    }
                    println!(
                        "{} Launched {} profile(s) side by side.",
                        "Summary:".bold(),
                        outcomes.len() - failures.len()
                    );
                }
                if !failures.is_empty() {
                    std::process::exit(1);
                }
            }

            Commands::Size { name } => {
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

                if cli.json {
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
                    println!(
                        "  Core data (sessions, logins, history): {}",
                        format_bytes(s.core_bytes)
                    );
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

            Commands::Backup {
                name,
                all,
                full,
                keep,
                encrypt,
                schedule,
                verify,
            } => {
                use crate::backup::{
                    create_backup, create_backup_all, verify_archive, AllBackupOutcome,
                };

                if let Some(action) = schedule {
                    let message = match action {
                        ScheduleAction::Install => {
                            crate::schedule::install(&storage, browser, keep)?
                        }
                        ScheduleAction::Uninstall => crate::schedule::uninstall(&storage, browser)?,
                        ScheduleAction::Status => crate::schedule::status(&storage, browser)?,
                    };
                    println!("{message}");
                    return Ok(());
                }

                if let Some(archive) = verify {
                    let passphrase = crate::backup::passphrase_to_open(&archive)?;
                    let report = verify_archive(&archive, passphrase.as_ref())?;
                    println!(
                        "{} {} is a valid bm backup",
                        "OK:".green().bold(),
                        archive.display().to_string().cyan().bold()
                    );
                    println!(
                        "  Profile:  {} ({})",
                        report.name.bold(),
                        report.browser.cyan()
                    );
                    println!(
                        "  Created:  {}",
                        report.created_at.format("%Y-%m-%d %H:%M:%S UTC")
                    );
                    match &report.proxy {
                        Some(p) => println!("  Proxy:    {}", p.to_string().cyan()),
                        None => println!("  Proxy:    {}", "none".dimmed()),
                    }
                    println!("  Made by:  bm {}", report.bm_version);
                    println!(
                        "  Contents: {} entr{}, {} decoded",
                        report.entries,
                        if report.entries == 1 { "y" } else { "ies" },
                        format_bytes(report.decoded_bytes)
                    );
                    return Ok(());
                }

                if !all && name.is_none() {
                    return Err(crate::error::BmError::Other(
                    "Specify a profile to back up, use --all for every profile of this browser, or --verify <archive>.".into(),
                ));
                }

                if all {
                    let passphrase = if encrypt {
                        Some(crate::backup::read_passphrase(true)?)
                    } else {
                        None
                    };
                    let outcomes =
                        create_backup_all(&storage, browser, full, keep, passphrase.as_ref())?;
                    let mut backed_up = 0;
                    let mut skipped = 0;
                    let mut failed = 0;
                    for outcome in outcomes {
                        match outcome {
                            AllBackupOutcome::BackedUp { name, outcome } => {
                                backed_up += 1;
                                println!(
                                    "{} '{}' → {} ({})",
                                    "ok".green().bold(),
                                    name.bold(),
                                    outcome.path.display(),
                                    format_bytes(outcome.archive_bytes)
                                );
                                if outcome.pruned > 0 {
                                    println!("    pruned {} older backup(s)", outcome.pruned);
                                }
                            }
                            AllBackupOutcome::SkippedRunning { name, pid } => {
                                skipped += 1;
                                match pid {
                                    Some(pid) => println!(
                                        "{} '{}' — browser is running (pid {}), skipped",
                                        "skip".yellow().bold(),
                                        name.bold(),
                                        pid
                                    ),
                                    None => println!(
                                        "{} '{}' — browser is running, skipped",
                                        "skip".yellow().bold(),
                                        name.bold()
                                    ),
                                }
                            }
                            AllBackupOutcome::Failed { name, error } => {
                                failed += 1;
                                println!("{} '{}' — {}", "fail".red().bold(), name.bold(), error);
                            }
                        }
                    }
                    println!(
                        "{} {} backed up, {} skipped, {} failed.",
                        "Summary:".bold(),
                        backed_up,
                        skipped,
                        failed
                    );
                    if failed > 0 {
                        std::process::exit(1);
                    }
                    return Ok(());
                }

                let name = name.as_ref().unwrap();
                let passphrase = if encrypt {
                    Some(crate::backup::read_passphrase(true)?)
                } else {
                    None
                };
                let outcome =
                    create_backup(&storage, browser, name, full, keep, passphrase.as_ref())?;
                println!(
                    "{} Snapshot saved to {}",
                    "Success:".green().bold(),
                    outcome.path.display().to_string().cyan().bold()
                );
                println!(
                    "  Archive: {} ({:.0}% of the profile's {})",
                    format_bytes(outcome.archive_bytes),
                    100.0 * outcome.archive_bytes as f64 / outcome.profile_bytes.max(1) as f64,
                    format_bytes(outcome.profile_bytes)
                );
                if outcome.pruned > 0 {
                    println!("  Pruned {} older backup(s) (--keep).", outcome.pruned);
                }
                println!(
                    "Restore it later with: {}",
                    format!("bm restore {}", outcome.path.display()).green()
                );
            }

            Commands::Restore { file, name, force } => {
                use crate::backup::restore_archive;

                let passphrase = crate::backup::passphrase_to_open(&file)?;
                let meta =
                    restore_archive(&storage, &file, name.as_deref(), force, passphrase.as_ref())?;
                println!(
                    "{} Restored profile '{}' for {}.",
                    "Success:".green().bold(),
                    meta.name.bold(),
                    browser.display_name().cyan()
                );
                println!(
                "Description and egress proxy came from the archive manifest; activate with {}.",
                format!("bm switch {}", meta.name).green()
            );
            }

            Commands::Status => {
                if cli.json {
                    print_json(&crate::report::status_json(&storage, browser)?)?;
                    return Ok(());
                }
                let report = collect_status(&storage, browser)?;
                print!("{}", render_status(&report));
            }

            Commands::Doctor { fix } => {
                use crate::doctor::{apply_fixes, check_browser_health, run_checks};

                if cli.json {
                    print_json(&crate::report::doctor_json(&storage, browser, fix)?)?;
                    return Ok(());
                }

                let health = check_browser_health(browser);
                match (&health.path, &health.source) {
                    (Some(path), Some(source)) => {
                        println!(
                            "{} {} binary resolved at {} ({})",
                            "Browser:".blue().bold(),
                            browser.display_name().cyan(),
                            path.display(),
                            source.to_string().green()
                        );
                    }
                    _ => {
                        println!(
                            "{} {} binary could not be located ({})",
                            "Browser:".yellow().bold(),
                            browser.display_name().cyan(),
                            health.error.as_deref().unwrap_or("not found").red()
                        );
                    }
                }

                let issues = run_checks(&storage, browser)?;
                if issues.is_empty() {
                    println!(
                        "{} All health checks passed for {}.",
                        "OK:".green().bold(),
                        browser.display_name().cyan()
                    );
                    return Ok(());
                }

                for issue in &issues {
                    let marker = if issue.repair.is_some() {
                        "fixable".cyan()
                    } else {
                        "report-only".dimmed()
                    };
                    println!(
                        "{} [{}] {} ({})",
                        "Issue:".yellow().bold(),
                        issue.check,
                        issue.detail,
                        marker
                    );
                }

                if fix {
                    let fixed = apply_fixes(&storage, &issues)?;
                    let remaining = issues.len() - fixed;
                    println!(
                    "{} Applied {} repair(s); {} issue(s) need manual attention (see report-only above).",
                    "Success:".green().bold(),
                    fixed,
                    remaining
                );
                } else {
                    let fixable = issues.iter().filter(|i| i.repair.is_some()).count();
                    if fixable > 0 {
                        println!(
                            "{} {} issue(s) can be repaired automatically — re-run with {}.",
                            "Note:".blue().bold(),
                            fixable,
                            "--fix".green()
                        );
                    }
                }
            }

            Commands::Completions { shell } => {
                use clap::CommandFactory;
                let mut cmd = Cli::command();
                clap_complete::generate(shell, &mut cmd, "bm", &mut std::io::stdout());
            }

            Commands::Mcp => {
                // stdio MCP server: stdout is protocol-only from here on.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        crate::error::BmError::Other(format!("cannot start async runtime: {e}"))
                    })?;
                rt.block_on(crate::mcp::serve(cli.browser))?;
            }

            Commands::Manpage => {
                use clap::CommandFactory;
                let cmd = Cli::command();
                clap_mangen::Man::new(cmd).render(&mut io::stdout())?;
            }

            Commands::Export { name, file } => {
                let out = transfer::export_profile(&storage, browser, &name, file.as_deref())?;
                println!(
                    "{} Configuration bundle written to {}",
                    "Success:".green().bold(),
                    out.display().to_string().cyan().bold()
                );
                println!("  Config only — no cookies, sessions, or logins. To move actual data between machines, use {} + {}.",
                "bm backup".green(), "bm restore".green());
            }

            Commands::Import { file, name, force } => {
                let meta = transfer::import_profile(&storage, &file, name.as_deref(), force)?;
                println!(
                    "{} Profile '{}' registered for {} from the bundle.",
                    "Success:".green().bold(),
                    meta.name.bold(),
                    meta.browser.display_name().cyan()
                );
                if meta.proxy.is_some() {
                    println!("  Egress proxy restored from the bundle.");
                }
                println!(
                "The profile dir is an empty placeholder — launch it with {} or make it the global default with {}.",
                format!("bm open {}", meta.name).green(),
                format!("bm switch {}", meta.name).green()
            );
            }

            Commands::Template { action } => match action {
                TemplateAction::Save {
                    name,
                    profile,
                    note,
                    force,
                } => {
                    let meta = template::save_template(
                        &storage,
                        browser,
                        &name,
                        &profile,
                        note.as_deref(),
                        force,
                    )?;
                    println!(
                        "{} Saved template '{}' from profile '{}' for {}.",
                        "Success:".green().bold(),
                        meta.name.bold(),
                        meta.source_profile.bold(),
                        browser.display_name().cyan()
                    );
                    println!(
                        "  Extracted components: {}",
                        if meta.components.is_empty() {
                            "none (source profile was clean/empty)".dimmed().to_string()
                        } else {
                            meta.components.join(", ").cyan().to_string()
                        }
                    );
                    println!(
                        "  Strict zero-session guarantee: cookies, logins, history, and website storage were stripped."
                    );
                }

                TemplateAction::List => {
                    let templates = template::list_templates(&storage, Some(browser))?;
                    if cli.json {
                        let json_val = serde_json::json!({
                            "browser": browser.to_string(),
                            "templates": templates,
                        });
                        println!("{}", serde_json::to_string_pretty(&json_val)?);
                        return Ok(());
                    }
                    if templates.is_empty() {
                        println!(
                            "No saved templates found for {}. Save one with '{}'.",
                            browser.display_name().cyan().bold(),
                            format!("bm template save <name> <profile> --browser {}", browser)
                                .green()
                        );
                        return Ok(());
                    }
                    println!(
                        "{} for {}:",
                        "Saved Templates".bold(),
                        browser.display_name().cyan().bold()
                    );
                    println!(
                        "  {:<18} {:<18} {:<28} {}",
                        "NAME".bold(),
                        "SOURCE PROFILE".bold(),
                        "COMPONENTS".bold(),
                        "NOTE".bold()
                    );
                    println!("  {}", "-".repeat(80).dimmed());
                    for tpl in templates {
                        let comps = if tpl.components.is_empty() {
                            "empty".dimmed().to_string()
                        } else {
                            tpl.components.join(", ")
                        };
                        println!(
                            "  {:<18} {:<18} {:<28} {}",
                            tpl.name.bold(),
                            tpl.source_profile.cyan(),
                            comps,
                            tpl.description.as_deref().unwrap_or("-").dimmed()
                        );
                    }
                }

                TemplateAction::Delete { name, yes } => {
                    let tpl_dir = storage.template_dir(&name);
                    if !tpl_dir.exists() {
                        return Err(BmError::TemplateNotFound { name });
                    }
                    if !yes && interactive {
                        print!(
                            "Are you sure you want to delete template '{}'? [y/N]: ",
                            name.yellow().bold()
                        );
                        io::stdout().flush()?;
                        let mut response = String::new();
                        io::stdin().read_line(&mut response)?;
                        if !response.trim().eq_ignore_ascii_case("y") {
                            println!("Aborted.");
                            return Ok(());
                        }
                    }
                    template::delete_template(&storage, &name)?;
                    println!(
                        "{} Deleted template '{}'.",
                        "Success:".green().bold(),
                        name.bold()
                    );
                }
            },

            Commands::Group { action } => match action {
                GroupAction::Create { name, note } => {
                    let g = group::create_group(&storage, &name, note.as_deref())?;
                    println!(
                        "{} Created session group '{}'.",
                        "Success:".green().bold(),
                        g.name.bold()
                    );
                    if let Some(desc) = g.description {
                        println!("  Description: {}", desc.dimmed());
                    }
                    println!(
                        "Add members with: {}",
                        format!("bm group add {} <profile> [--browser <b>]", g.name).green()
                    );
                }

                GroupAction::Add {
                    group,
                    profile,
                    browser: target_browser,
                    url,
                    proxy,
                } => {
                    let b = target_browser.unwrap_or(browser);
                    let proxy_cfg = match proxy {
                        Some(p) => Some(ProxyConfig::parse(&p, None)?),
                        None => None,
                    };
                    let g = group::add_member(&storage, &group, b, &profile, url, proxy_cfg)?;
                    println!(
                        "{} Added '{}' ({}) to group '{}' ({} total members).",
                        "Success:".green().bold(),
                        profile.bold(),
                        b.display_name().cyan(),
                        g.name.bold(),
                        g.members.len()
                    );
                }

                GroupAction::Remove {
                    group,
                    profile,
                    browser: target_browser,
                } => {
                    let b = target_browser.unwrap_or(browser);
                    let g = group::remove_member(&storage, &group, b, &profile)?;
                    println!(
                        "{} Removed '{}' ({}) from group '{}' ({} remaining).",
                        "Success:".green().bold(),
                        profile.bold(),
                        b.display_name().cyan(),
                        g.name.bold(),
                        g.members.len()
                    );
                }

                GroupAction::Delete { name, yes } => {
                    if !storage.groups_file().exists() {
                        return Err(BmError::GroupNotFound { name });
                    }
                    if !yes && interactive {
                        print!(
                            "Are you sure you want to delete session group '{}'? [y/N]: ",
                            name.yellow().bold()
                        );
                        io::stdout().flush()?;
                        let mut response = String::new();
                        io::stdin().read_line(&mut response)?;
                        if !response.trim().eq_ignore_ascii_case("y") {
                            println!("Aborted.");
                            return Ok(());
                        }
                    }
                    group::delete_group(&storage, &name)?;
                    println!(
                        "{} Deleted session group '{}'.",
                        "Success:".green().bold(),
                        name.bold()
                    );
                }

                GroupAction::List => {
                    let groups = group::list_groups(&storage)?;
                    if cli.json {
                        let val = serde_json::json!({
                            "groups": groups,
                        });
                        println!("{}", serde_json::to_string_pretty(&val)?);
                        return Ok(());
                    }
                    if groups.is_empty() {
                        println!(
                            "No session groups found. Create one with '{}'.",
                            "bm group create <name>".green()
                        );
                        return Ok(());
                    }
                    println!("{}:", "Session Groups".bold());
                    println!(
                        "  {:<18} {:<10} {}",
                        "GROUP".bold(),
                        "MEMBERS".bold(),
                        "DESCRIPTION".bold()
                    );
                    println!("  {}", "-".repeat(60).dimmed());
                    for g in groups {
                        println!(
                            "  {:<18} {:<10} {}",
                            g.name.bold(),
                            g.members.len(),
                            g.description.as_deref().unwrap_or("-").dimmed()
                        );
                    }
                }

                GroupAction::Show { name } => {
                    let g = group::get_group(&storage, &name)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&g)?);
                        return Ok(());
                    }
                    println!("Group: {}", g.name.bold().green());
                    if let Some(desc) = g.description {
                        println!("Description: {}", desc.dimmed());
                    }
                    println!("Members: ({})", g.members.len());
                    if g.members.is_empty() {
                        println!(
                            "  {} No members in this group. Add with '{}'.",
                            "-".dimmed(),
                            format!("bm group add {} <profile>", g.name).green()
                        );
                        return Ok(());
                    }
                    println!(
                        "  {:<14} {:<18} {:<24} {}",
                        "BROWSER".bold(),
                        "PROFILE".bold(),
                        "PROXY".bold(),
                        "URLS".bold()
                    );
                    println!("  {}", "-".repeat(70).dimmed());
                    for m in g.members {
                        let proxy_str = m
                            .proxy
                            .map(|p| p.to_string())
                            .or_else(|| {
                                storage
                                    .proxy_for(m.browser, &m.profile)
                                    .map(|p| format!("{} (inherited)", p))
                            })
                            .unwrap_or_else(|| "-".dimmed().to_string());
                        let urls_str = if m.urls.is_empty() {
                            "-".dimmed().to_string()
                        } else {
                            m.urls.join(", ")
                        };
                        println!(
                            "  {:<14} {:<18} {:<24} {}",
                            m.browser.display_name().cyan(),
                            m.profile.bold(),
                            proxy_str,
                            urls_str
                        );
                    }
                }

                GroupAction::Status { name } => {
                    let reports = match name {
                        Some(ref n) => vec![group::status_group(&storage, n)?],
                        None => group::status_all_groups(&storage)?,
                    };
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&reports)?);
                        return Ok(());
                    }
                    if reports.is_empty() {
                        println!(
                            "No session groups found. Create one with '{}'.",
                            "bm group create <name>".green()
                        );
                        return Ok(());
                    }
                    println!("{}:", "Group Runtime Status".bold());
                    for r in reports {
                        println!(
                            "\nGroup: {} ({} of {} running)",
                            r.name.bold().green(),
                            r.running_count.to_string().cyan(),
                            r.member_count
                        );
                        if let Some(desc) = r.description {
                            println!("  Description: {}", desc.dimmed());
                        }
                        if r.members.is_empty() {
                            println!("  {} No members.", "-".dimmed());
                            continue;
                        }
                        println!(
                            "  {:<14} {:<18} {:<12} {:<10} {:<20} {}",
                            "BROWSER".bold(),
                            "PROFILE".bold(),
                            "STATUS".bold(),
                            "PID".bold(),
                            "PROXY".bold(),
                            "CDP".bold()
                        );
                        println!("  {}", "-".repeat(85).dimmed());
                        for m in r.members {
                            let status_str = if m.running {
                                "running".green().bold().to_string()
                            } else {
                                "stopped".dimmed().to_string()
                            };
                            let pid_str = m
                                .pid
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "-".dimmed().to_string());
                            let proxy_str = m.proxy.unwrap_or_else(|| "-".dimmed().to_string());
                            let cdp_str =
                                m.cdp_endpoint.unwrap_or_else(|| "-".dimmed().to_string());
                            println!(
                                "  {:<14} {:<18} {:<12} {:<10} {:<20} {}",
                                m.browser.display_name().cyan(),
                                m.profile.bold(),
                                status_str,
                                pid_str,
                                proxy_str,
                                cdp_str
                            );
                        }
                    }
                }

                GroupAction::Open { name, urls, cdp } => {
                    if cdp {
                        println!(
                            "{} --cdp hands FULL control of all launched group browser instances — loopback only.",
                            "Warning:".yellow().bold()
                        );
                    }
                    let report = group::open_group(&storage, &name, &urls, cdp)?;
                    println!(
                        "{} Launching group '{}' ({} members):",
                        "Group:".bold(),
                        report.group.green().bold(),
                        report.members.len()
                    );
                    for m in &report.members {
                        match &m.error {
                            None => {
                                let pid_str =
                                    m.pid.map(|p| format!("(pid {})", p)).unwrap_or_default();
                                println!(
                                    "  {} {}/{} {} spawned detached.",
                                    "ok".green().bold(),
                                    m.browser.display_name().cyan(),
                                    m.profile.bold(),
                                    pid_str.dimmed()
                                );
                                if let Some(endpoint) = &m.cdp_endpoint {
                                    println!("    CDP endpoint: {}", endpoint.cyan().bold());
                                }
                            }
                            Some(err) => {
                                println!(
                                    "  {} {}/{} — {}",
                                    "fail".red().bold(),
                                    m.browser.display_name().cyan(),
                                    m.profile.bold(),
                                    err
                                );
                            }
                        }
                    }
                }

                GroupAction::Close { name, force } => {
                    let report = group::close_group(&storage, &name, force)?;
                    println!(
                        "{} Closing group '{}' ({} members):",
                        "Group:".bold(),
                        report.group.green().bold(),
                        report.members.len()
                    );
                    for m in &report.members {
                        if m.success {
                            println!(
                                "  {} {}/{} — closed ({})",
                                "ok".green().bold(),
                                m.browser.display_name().cyan(),
                                m.profile.bold(),
                                m.method.dimmed()
                            );
                        } else {
                            println!(
                                "  {} {}/{} — {}",
                                "fail".red().bold(),
                                m.browser.display_name().cyan(),
                                m.profile.bold(),
                                m.error.as_deref().unwrap_or("failed to close")
                            );
                        }
                    }
                }
            },

            Commands::Cdp { action } => match action {
                CdpAction::Status => {
                    let purged = crate::cdp::purge_dead_entries(&storage)?;
                    if purged > 0 {
                        println!("Purged {} dead endpoint(s).", purged);
                    }
                    let registry = crate::cdp::load_registry(&storage);
                    if registry.is_empty() {
                        println!(
                            "No CDP endpoints. Start one with {}.",
                            "bm open <name> --cdp".green()
                        );
                        return Ok(());
                    }
                    println!("{}:", "Live CDP endpoints".bold());
                    for (key, entry) in registry {
                        if crate::cdp::port_alive(entry.port) {
                            println!(
                                "  {} → {} (since {})",
                                key.bold(),
                                entry.ws_url().cyan(),
                                entry.started_at.format("%Y-%m-%d %H:%M")
                            );
                        }
                    }
                    println!(
                        "Close one with {} — a graceful Browser.close, never SIGKILL.",
                        "bm cdp close <profile>".green()
                    );
                }
                CdpAction::Close { name } => {
                    let entry = crate::cdp::load_registry(&storage)
                        .remove(&format!("{browser}:{name}"))
                        .ok_or_else(|| {
                            crate::error::BmError::Other(format!(
                                "no CDP endpoint recorded for {browser}:{name} — check {}",
                                "bm cdp status".green()
                            ))
                        })?;
                    if !crate::cdp::port_alive(entry.port) {
                        crate::cdp::remove_entry(&storage, browser, &name)?;
                        println!("Endpoint was already dead — registry entry removed.");
                        return Ok(());
                    }
                    crate::cdp::close_via_browser_close(&entry)?;
                    // Wait for the browser to actually release the port.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while crate::cdp::port_alive(entry.port) && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    crate::cdp::remove_entry(&storage, browser, &name)?;
                    if crate::cdp::port_alive(entry.port) {
                        println!(
                            "{} Browser.close was acknowledged but 127.0.0.1:{} is still accepting connections — check the browser.",
                            "Warning:".yellow().bold(),
                            entry.port
                        );
                    } else {
                        println!(
                            "{} Browser closed cleanly via CDP (127.0.0.1:{}).",
                            "Success:".green().bold(),
                            entry.port
                        );
                    }
                }
            },

            Commands::Proxy { action } => match action {
                ProxyAction::Set {
                    name,
                    server,
                    bypass,
                } => {
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
                    if cli.json {
                        print_json(&crate::report::proxy_json(
                            &storage,
                            browser,
                            name.as_deref(),
                        )?)?;
                        return Ok(());
                    }
                    let profiles = storage.list_profiles(browser)?;
                    match name {
                        Some(name) => {
                            let (meta, is_active) = profiles
                                .into_iter()
                                .find(|(m, _)| m.name == name)
                                .ok_or_else(|| crate::error::BmError::ProfileNotFound {
                                    name: name.clone(),
                                    browser: browser.display_name().to_string(),
                                })?;
                            println!(
                                "Proxy for {} profile '{}'{}:",
                                browser.display_name().cyan(),
                                if is_active {
                                    meta.name.green().bold().to_string()
                                } else {
                                    meta.name.bold().to_string()
                                },
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
                            println!(
                                "  {:<18} {:<28} {}",
                                "NAME".bold(),
                                "SERVER".bold(),
                                "BYPASS".bold()
                            );
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
                    use crate::proxy_test::{check_egress, resolve_targets, UreqFetcher};
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

            Commands::WatchTemp { pid, path } => {
                crate::open::watch_and_purge(pid, &path);
                return Ok(());
            }
        },
    }

    Ok(())
}

/// Resolve `bm switch <name>` where `-` means "the previous profile"
/// (recorded in ~/.bm/state.json on every successful switch). `-` is not a
/// valid profile name, so there is no ambiguity.
fn resolve_switch_name(raw: &str, previous: Option<String>) -> Result<String> {
    if raw == "-" {
        previous.ok_or_else(|| {
            crate::error::BmError::Other(
                "No previous profile recorded yet — switch once, then `bm switch -` toggles back."
                    .into(),
            )
        })
    } else {
        Ok(raw.to_string())
    }
}

/// The shared switch flow: swap the symlink, sync the global-launch proxy
/// (managed policy on macOS, .desktop override on Linux), and print the
/// followup notes. Used by `bm switch <name>` and the interactive picker.
fn switch_to(storage: &StorageManager, browser: Browser, name: &str, force: bool) -> Result<()> {
    println!(
        "Switching {} profile to '{}'...",
        browser.display_name().cyan(),
        name.bold()
    );

    let backup = storage.switch_profile(name, browser, force)?;

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

    policy::sync_for_active_profile(storage, browser)?;
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
            warn_side_by_side_override(storage, browser, p);
        }
    }
    Ok(())
}

/// Interactive profile picker (TTY only): the active profile is preselected
/// and descriptions render as hint lines. Returns `None` when the user
/// cancels with Esc/Ctrl-C — no state changes.
fn pick_profile(storage: &StorageManager, browser: Browser) -> Result<Option<String>> {
    let profiles = storage.list_profiles(browser)?;
    if profiles.is_empty() {
        println!(
            "No profiles found for {}. Create one with 'bm create <name>'.",
            browser.display_name()
        );
        return Ok(None);
    }

    let active = storage.get_current_active(browser)?.unwrap_or_default();
    let options: Vec<(String, String)> = profiles
        .iter()
        .map(|(m, _)| {
            let display = if m.description.is_empty() {
                m.name.clone()
            } else {
                format!("{} — {}", m.name, m.description)
            };
            (m.name.clone(), display)
        })
        .collect();
    let starting_cursor = profiles
        .iter()
        .position(|(m, _)| m.name == active)
        .unwrap_or(0);

    let selection = inquire::Select::new(
        "Switch to which profile?",
        options.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
    )
    .with_starting_cursor(starting_cursor)
    .with_help_message("Enter to switch · Esc to cancel")
    .prompt();

    match selection {
        Ok(choice) => Ok(options
            .into_iter()
            .find(|(_, display)| *display == choice)
            .map(|(name, _)| name)),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(e) => Err(BmError::Other(format!("profile picker failed: {e}"))),
    }
}

/// Pretty-print a JSON value for `--json` consumers (single trailing
/// newline; nothing else may pollute stdout).
fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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

fn is_active_profile(
    storage: &StorageManager,
    browser: crate::browser::Browser,
    name: &str,
) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_dash_resolves_previous_profile() {
        assert_eq!(resolve_switch_name("work", None).unwrap(), "work");
        assert_eq!(
            resolve_switch_name("-", Some("work".into())).unwrap(),
            "work"
        );
        assert!(resolve_switch_name("-", None).is_err());
    }
}
