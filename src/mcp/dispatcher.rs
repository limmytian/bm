use serde_json::{json, Value};

use crate::browser::Browser;
use crate::error::BmError;
use crate::storage::StorageManager;

/// Resolve the effective browser for a tool call: the explicit `browser`
/// argument when present, else the server's `--browser` default.
pub(crate) fn pick_browser(default_browser: Browser, args: &Value) -> Result<Browser, BmError> {
    match args.get("browser").and_then(Value::as_str) {
        Some(raw) => parse_browser(raw),
        None => Ok(default_browser),
    }
}

pub(crate) fn parse_browser(raw: &str) -> Result<Browser, BmError> {
    match raw {
        "chrome" => Ok(Browser::Chrome),
        "edge" => Ok(Browser::Edge),
        "brave" => Ok(Browser::Brave),
        "chromium" => Ok(Browser::Chromium),
        other => Err(BmError::Other(format!(
            "unsupported browser '{other}' (chrome, edge, brave, chromium)"
        ))),
    }
}

/// Run blocking bm logic off the async reactor.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, BmError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BmError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BmError::Other(format!("task panicked: {e}")))?
}

/// Dispatch one tool call by name. Returns the structured JSON payload.
pub async fn dispatch_tool(
    default_browser: Browser,
    name: &str,
    args: Value,
) -> Result<Value, BmError> {
    let args = if args.is_null() { json!({}) } else { args };
    match name {
        "list_profiles" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::list_json(&storage, browser)
            })
            .await
        }
        "current" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::current_json(&storage, browser)
            })
            .await
        }
        "status" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::status_json(&storage, browser)
            })
            .await
        }
        "open_profile" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let urls = args
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| BmError::Other("urls must be strings".into()))
                })
                .transpose()?
                .unwrap_or_default();
            let cdp = args.get("cdp").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let outcomes =
                    crate::open::OpenLauncher::launch_all(&storage, browser, &[name], &urls, cdp)?;
                let outcomes: Vec<Value> = outcomes
                    .iter()
                    .map(|o| {
                        json!({
                            "profile": o.profile,
                            "launched": o.error.is_none(),
                            "error": o.error,
                            "cdp_endpoint": o.cdp_endpoint,
                            "cdp_reused": o.cdp_reused,
                        })
                    })
                    .collect();
                Ok(json!({"outcomes": outcomes}))
            })
            .await
        }
        "switch" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let backup = storage.switch_profile(&name, browser, force)?;
                crate::policy::sync_for_active_profile(&storage, browser)?;
                Ok(json!({
                    "switched_to": name,
                    "previous_data_preserved_as": backup,
                }))
            })
            .await
        }
        "proxy_get" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args.get("name").and_then(Value::as_str).map(str::to_string);
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::proxy_json(&storage, browser, name.as_deref())
            })
            .await
        }
        "proxy_set" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let server_str = args
                .get("server")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: server".into()))?
                .to_string();
            let bypass = args
                .get("bypass")
                .and_then(Value::as_str)
                .map(str::to_string);
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::lease::guard_lease(&storage, browser, &name, false)?;
                let cfg = crate::proxy::ProxyConfig::parse(&server_str, bypass.as_deref())?;
                let meta = storage.set_proxy(&name, browser, Some(cfg))?;
                let is_active =
                    storage.get_current_active(browser)?.as_deref() == Some(meta.name.as_str());
                if is_active {
                    crate::policy::sync_for_active_profile(&storage, browser)?;
                }
                Ok(json!({
                    "name": meta.name,
                    "proxy": meta.proxy.as_ref().map(|p| p.to_string()),
                    "active_profile": is_active,
                }))
            })
            .await
        }
        "proxy_clear" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::lease::guard_lease(&storage, browser, &name, false)?;
                let meta = storage.set_proxy(&name, browser, None)?;
                let is_active =
                    storage.get_current_active(browser)?.as_deref() == Some(meta.name.as_str());
                if is_active {
                    crate::policy::sync_for_active_profile(&storage, browser)?;
                }
                Ok(json!({"name": meta.name, "proxy": Value::Null, "active_profile": is_active}))
            })
            .await
        }
        "backup" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let keep = args.get("keep").and_then(Value::as_u64).map(|k| k as usize);
            let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::lease::guard_lease(&storage, browser, &name, false)?;
                let outcome =
                    crate::backup::create_backup(&storage, browser, &name, full, keep, None)?;
                Ok(json!({
                    "path": outcome.path.display().to_string(),
                    "archive_bytes": outcome.archive_bytes,
                    "profile_bytes": outcome.profile_bytes,
                    "pruned": outcome.pruned,
                }))
            })
            .await
        }
        "export_session" => {
            let browser = pick_browser(default_browser, &args)?;
            let profile = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument 'name'".into()))?
                .to_string();
            let domain = args
                .get("domain")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument 'domain'".into()))?
                .to_string();
            let allow_raw = args
                .get("allow_raw")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            blocking(move || {
                let storage = StorageManager::new()?;
                let bundle = crate::session::export_session(&storage, browser, &profile, &domain)?;
                if allow_raw {
                    Ok(serde_json::to_value(&bundle)?)
                } else {
                    let masked_cookies: Vec<_> = bundle
                        .cookies
                        .iter()
                        .map(|c| {
                            json!({
                                "name": c.name,
                                "domain": c.domain,
                                "path": c.path,
                                "value": c.masked_value(),
                                "expires_utc": c.expires_utc,
                                "is_secure": c.is_secure,
                                "is_httponly": c.is_httponly,
                                "samesite": c.samesite,
                            })
                        })
                        .collect();
                    Ok(json!({
                        "version": bundle.version,
                        "browser": bundle.browser,
                        "profile": bundle.profile,
                        "domain": bundle.domain,
                        "exported_at": bundle.exported_at,
                        "cookies": masked_cookies,
                        "note": "Values masked for security; set allow_raw=true to request plaintext"
                    }))
                }
            })
            .await
        }
        "group_list" => {
            blocking(move || {
                let storage = StorageManager::new()?;
                let reports = crate::group::status_all_groups(&storage)?;
                Ok(serde_json::to_value(&reports)?)
            })
            .await
        }
        "group_open" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let cdp = args.get("cdp").and_then(Value::as_bool).unwrap_or(false);
            let urls = args
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| BmError::Other("urls must be strings".into()))
                })
                .transpose()?
                .unwrap_or_default();
            blocking(move || {
                let storage = StorageManager::new()?;
                let report = crate::group::open_group(&storage, &name, &urls, cdp)?;
                Ok(serde_json::to_value(&report)?)
            })
            .await
        }
        "group_close" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let report = crate::group::close_group(&storage, &name, force)?;
                Ok(serde_json::to_value(&report)?)
            })
            .await
        }
        "group_create" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
            let members: Vec<String> = args
                .get("members")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| BmError::Other("members must be strings".into()))
                })
                .transpose()?
                .unwrap_or_default();
            blocking(move || {
                let storage = StorageManager::new()?;
                let mut g = crate::group::create_group(&storage, &name, description.as_deref())?;
                for m in members {
                    let (b, profile) = if let Some((b_str, p_str)) = m.split_once(':') {
                        (parse_browser(b_str)?, p_str)
                    } else {
                        (default_browser, m.as_str())
                    };
                    g = crate::group::add_member(
                        &storage,
                        &name,
                        b,
                        profile,
                        Vec::new(),
                        None,
                    )?;
                }
                Ok(serde_json::to_value(&g)?)
            })
            .await
        }
        "group_delete" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::group::delete_group(&storage, &name)?;
                Ok(json!({"deleted": name}))
            })
            .await
        }
        "lease_acquire" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let holder = args
                .get("holder")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: holder".into()))?
                .to_string();
            let ttl = args.get("ttl").and_then(Value::as_u64);
            let purpose = args
                .get("purpose")
                .and_then(Value::as_str)
                .map(str::to_string);
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let opts = crate::lease::ClaimOptions {
                    holder: &holder,
                    ttl_secs: ttl,
                    reason: purpose.as_deref(),
                    force,
                    pid: None,
                };
                let lease = crate::lease::claim_lease(&storage, browser, &name, opts)?;
                Ok(serde_json::to_value(&lease)?)
            })
            .await
        }
        "lease_release" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let holder = args
                .get("holder")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: holder".into()))?
                .to_string();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::lease::release_lease(&storage, browser, &name, Some(&holder), force)?;
                Ok(json!({"released": name, "holder": holder}))
            })
            .await
        }
        "lease_renew" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let ttl = args.get("ttl").and_then(Value::as_u64);
            blocking(move || {
                let storage = StorageManager::new()?;
                let lease = crate::lease::heartbeat_lease(&storage, browser, &name, ttl)?;
                Ok(serde_json::to_value(&lease)?)
            })
            .await
        }
        "lease_list" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                let leases = crate::lease::list_leases(&storage, Some(browser), None)?;
                Ok(serde_json::to_value(&leases)?)
            })
            .await
        }
        "create_profile" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let note = args.get("note").and_then(Value::as_str).map(str::to_string);
            let template = args
                .get("template")
                .and_then(Value::as_str)
                .map(str::to_string);
            blocking(move || {
                let storage = StorageManager::new()?;
                let (meta, applied_template) = storage.create_profile_with_template(
                    &name,
                    note.as_deref(),
                    browser,
                    template.as_deref(),
                )?;
                Ok(json!({
                    "name": meta.name,
                    "browser": meta.browser,
                    "description": meta.description,
                    "created_at": meta.created_at,
                    "updated_at": meta.updated_at,
                    "template": applied_template.map(|t| t.name),
                }))
            })
            .await
        }
        "clone_profile" => {
            let browser = pick_browser(default_browser, &args)?;
            let src = args
                .get("src")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: src".into()))?
                .to_string();
            let dst = args
                .get("dst")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: dst".into()))?
                .to_string();
            let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
            let no_proxy = args
                .get("no_proxy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let (meta, bytes_copied) = storage.clone_profile(&src, &dst, browser, full, !no_proxy)?;
                Ok(json!({
                    "name": meta.name,
                    "browser": meta.browser,
                    "description": meta.description,
                    "bytes_copied": bytes_copied,
                    "created_at": meta.created_at,
                }))
            })
            .await
        }
        "delete_profile" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                storage.delete_profile(&name, browser)?;
                Ok(json!({"deleted": name}))
            })
            .await
        }
        "open_temp_profile" => {
            let browser = pick_browser(default_browser, &args)?;
            let template = args
                .get("template")
                .and_then(Value::as_str)
                .map(str::to_string);
            let urls = args
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| BmError::Other("urls must be strings".into()))
                })
                .transpose()?
                .unwrap_or_default();
            let cdp = args.get("cdp").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
                let outcome = crate::open::OpenLauncher::launch_temp(
                    &storage,
                    browser,
                    template.as_deref(),
                    &urls,
                    cdp,
                )?;
                Ok(json!({
                    "temp_id": outcome.temp_id,
                    "temp_dir": outcome.temp_dir.display().to_string(),
                    "browser_pid": outcome.browser_pid,
                    "cdp_endpoint": outcome.cdp_endpoint,
                }))
            })
            .await
        }
        "template_list" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                let templates = crate::template::list_templates(&storage, Some(browser))?;
                Ok(serde_json::to_value(&templates)?)
            })
            .await
        }
        "template_inspect" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                let all = crate::template::list_templates(&storage, None)?;
                let found = all
                    .into_iter()
                    .find(|t| t.name == name)
                    .ok_or(BmError::TemplateNotFound { name })?;
                Ok(serde_json::to_value(&found)?)
            })
            .await
        }
        "cdp_list" => {
            blocking(move || {
                let storage = StorageManager::new()?;
                let _ = crate::cdp::purge_dead_entries(&storage)?;
                let registry = crate::cdp::load_registry(&storage);
                let mut list = Vec::new();
                for (key, entry) in registry {
                    if crate::cdp::port_alive(entry.port) {
                        list.push(json!({
                            "key": key,
                            "port": entry.port,
                            "ws_url": entry.ws_url(),
                            "pid": entry.pid,
                            "started_at": entry.started_at,
                        }));
                    }
                }
                Ok(json!({"endpoints": list}))
            })
            .await
        }
        "cdp_close" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                let key = format!("{browser}:{name}");
                let mut registry = crate::cdp::load_registry(&storage);
                let entry = registry.remove(&key).ok_or_else(|| {
                    BmError::Other(format!("no CDP endpoint recorded for {key}"))
                })?;
                if crate::cdp::port_alive(entry.port) {
                    crate::cdp::close_via_browser_close(&entry)?;
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while crate::cdp::port_alive(entry.port)
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                crate::cdp::remove_entry(&storage, browser, &name)?;
                Ok(json!({
                    "closed": name,
                    "browser": browser,
                    "still_alive": crate::cdp::port_alive(entry.port),
                }))
            })
            .await
        }
        "doctor_diagnose" => {
            let browser = pick_browser(default_browser, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::doctor_json(&storage, browser, false)
            })
            .await
        }
        "proxy_test" => {
            let browser = pick_browser(default_browser, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
                let targets =
                    crate::proxy_test::resolve_targets(&storage, browser, Some(&name))?;
                let (_, proxy_opt) = targets
                    .into_iter()
                    .next()
                    .ok_or_else(|| BmError::ProfileNotFound {
                        name: name.clone(),
                        browser: browser.display_name().to_string(),
                    })?;
                if let Some(proxy) = proxy_opt {
                    let fetcher = crate::proxy_test::UreqFetcher;
                    let check = crate::proxy_test::check_egress(
                        &fetcher,
                        "https://api.ipify.org",
                        &proxy,
                        std::time::Duration::from_secs(5),
                    );
                    Ok(serde_json::to_value(&check)?)
                } else {
                    Ok(json!({
                        "profile": name,
                        "has_proxy": false,
                        "effective": false,
                        "note": "profile has no proxy configured (direct connection)"
                    }))
                }
            })
            .await
        }
        other => Err(BmError::Other(format!("unknown tool '{other}'"))),
    }
}
