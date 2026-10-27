//! `bm mcp`: a built-in stdio MCP server wrapping existing bm modules.
//!
//! Thin wrappers by design — every tool delegates to the same functions the
//! CLI uses (storage/open/switch/backup/report) and returns the same stable
//! JSON shapes documented in docs/json.md. No new logic lives here beyond
//! argument plumbing; the running-browser and active-profile guards of the
//! underlying modules apply unchanged.
//!
//! Transport: stdio only for v1 (newline-delimited JSON-RPC 2.0, per the
//! MCP spec). **stdout carries protocol frames and nothing else** — bm's
//! human output never runs during tool calls, and anything that must warn
//! goes to stderr (or is dropped). Remote HTTP transports are out of scope.
//!
//! SDK choice (rmcp 3.8, the official Rust MCP SDK) over hand-rolled
//! JSON-RPC: the initialize handshake, protocol-version negotiation, and
//! tool-result envelopes are exactly the parts a hand-rolled server gets
//! subtly wrong against real hosts (Claude Desktop, Cursor, ZCode), and
//! rmcp tracks spec revisions for us. The tokio dependency it brings is
//! the accepted cost.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::ErrorData as McpError;
use rmcp::{model::ServerCapabilities, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};

use crate::browser::Browser;
use crate::error::BmError;
use crate::storage::StorageManager;

pub struct BmMcpServer {
    default_browser: Browser,
}

impl BmMcpServer {
    pub fn new(default_browser: Browser) -> Self {
        Self { default_browser }
    }
}

/// Resolve the effective browser for a tool call: the explicit `browser`
/// argument when present, else the server's `--browser` default.
fn pick_browser(server: &BmMcpServer, args: &Value) -> Result<Browser, BmError> {
    match args.get("browser").and_then(Value::as_str) {
        Some(raw) => parse_browser(raw),
        None => Ok(server.default_browser),
    }
}

fn parse_browser(raw: &str) -> Result<Browser, BmError> {
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
async fn blocking<T, F>(f: F) -> Result<T, BmError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BmError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BmError::Other(format!("task panicked: {e}")))?
}

/// The tool catalog: name, model-facing description, and a precise JSON
/// Schema for the input. Schemas are hand-written here (the model-facing
/// contract) — keep them in sync with the handlers below.
fn tool_definitions() -> Vec<Tool> {
    /// Build an input schema: object type with the given properties and
    /// required keys, closed to additional properties.
    fn schema(properties: Value, required: &[&str]) -> Arc<JsonObject> {
        let mut map = JsonObject::new();
        map.insert("type".into(), json!("object"));
        map.insert("properties".into(), properties);
        if !required.is_empty() {
            map.insert(
                "required".into(),
                Value::Array(required.iter().map(|r| json!(r)).collect()),
            );
        }
        map.insert("additionalProperties".into(), json!(false));
        Arc::new(map)
    }

    let browser_enum = json!({
        "type": "string",
        "enum": ["chrome", "edge", "brave", "chromium"],
        "description": "Target browser; defaults to the server's --browser value"
    });

    vec![
        Tool::new("list_profiles", "List the managed browser profiles and their state (name, description, active flag, proxy, timestamps)", schema(json!({"properties": {"browser": browser_enum}}), &[])),
        Tool::new("current", "Show the currently active profile for a browser (null when the browser runs on unmanaged data)", schema(json!({"properties": {"browser": browser_enum}}), &[])),
        Tool::new("status", "One-screen overview: active profile, which profiles have a live browser process (with pid), per-profile proxy, newest backup, and open doctor findings", schema(json!({"properties": {"browser": browser_enum}}), &[])),
        Tool::new("open_profile", "Launch one or more profiles side by side with their own user-data-dir and proxy flags (global default profile unchanged). With cdp=true, enables Chrome DevTools Protocol on a loopback, OS-assigned port and returns the ws endpoint — full browser control, only when explicitly requested", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to launch; must exist"},
            "browser": browser_enum,
            "urls": {"type": "array", "items": {"type": "string"}, "description": "URLs to open in every launched profile"},
            "cdp": {"type": "boolean", "description": "Enable CDP for this launch (loopback only, endpoint returned; the profile must not already be running without CDP)"}
        }}), &["name"])),
        Tool::new("switch", "Switch the browser's globally active profile via the default-data-dir symlink (refuses while the browser runs unless force=true; a pre-existing real data dir is auto-backed up as profile 'original')", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to activate"},
            "browser": browser_enum,
            "force": {"type": "boolean", "description": "Terminate running browser instances if necessary"}
        }}), &["name"])),
        Tool::new("proxy_get", "Show the per-profile egress proxy configuration (all profiles, or one)", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to query; omit for all profiles"},
            "browser": browser_enum
        }}), &[])),
        Tool::new("proxy_set", "Set a profile's egress proxy (socks5, http, or https; honored by standalone launches, and by global launches through managed policy when the profile is active)", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to configure"},
            "server": {"type": "string", "description": "Proxy as <scheme>://<host>:<port>, e.g. socks5://127.0.0.1:1080"},
            "bypass": {"type": "string", "description": "Comma- or semicolon-separated bypass hosts, e.g. \"localhost,*.lan\""},
            "browser": browser_enum
        }}), &["name", "server"])),
        Tool::new("proxy_clear", "Remove a profile's egress proxy configuration", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to clear"},
            "browser": browser_enum
        }}), &["name"])),
        Tool::new("backup", "Create a local snapshot of a profile (tar+zstd, caches excluded) under ~/.bm/backups; refuses while the browser is running", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to back up"},
            "browser": browser_enum,
            "keep": {"type": "integer", "minimum": 1, "description": "Keep only the N newest backups of this profile"},
            "full": {"type": "boolean", "description": "Include regenerable caches"}
        }}), &["name"])),
    ]
}

/// One tool call, dispatched by name. Returns the structured JSON payload.
async fn dispatch_tool(server: &BmMcpServer, name: &str, args: Value) -> Result<Value, BmError> {
    let args = if args.is_null() { json!({}) } else { args };
    match name {
        "list_profiles" => {
            let browser = pick_browser(server, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::list_json(&storage, browser)
            })
            .await
        }
        "current" => {
            let browser = pick_browser(server, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::current_json(&storage, browser)
            })
            .await
        }
        "status" => {
            let browser = pick_browser(server, &args)?;
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::status_json(&storage, browser)
            })
            .await
        }
        "open_profile" => {
            let browser = pick_browser(server, &args)?;
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
            let browser = pick_browser(server, &args)?;
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
            let browser = pick_browser(server, &args)?;
            let name = args.get("name").and_then(Value::as_str).map(str::to_string);
            blocking(move || {
                let storage = StorageManager::new()?;
                crate::report::proxy_json(&storage, browser, name.as_deref())
            })
            .await
        }
        "proxy_set" => {
            let browser = pick_browser(server, &args)?;
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
            let browser = pick_browser(server, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            blocking(move || {
                let storage = StorageManager::new()?;
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
            let browser = pick_browser(server, &args)?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| BmError::Other("missing required argument: name".into()))?
                .to_string();
            let keep = args.get("keep").and_then(Value::as_u64).map(|k| k as usize);
            let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || {
                let storage = StorageManager::new()?;
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
        other => Err(BmError::Other(format!("unknown tool '{other}'"))),
    }
}

impl ServerHandler for BmMcpServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "bm manages Chromium-family browser profiles (Chrome, Edge, Brave, Chromium) with \
             isolated user-data-dirs, per-profile egress proxies, and local snapshots. \
             Read-only tools (list_profiles, current, status, proxy_get) are safe to call freely; \
             open_profile launches browsers; switch/proxy_set/proxy_clear/backup mutate state and \
             refuse while the target browser is running (switch has force).",
            );
        info.server_info = rmcp::model::Implementation::new("bm", env!("CARGO_PKG_VERSION"));
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tool_definitions()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = request
            .arguments
            .map(|m| Value::Object(m.into_iter().collect()))
            .unwrap_or(Value::Null);
        match dispatch_tool(self, &request.name, args).await {
            Ok(value) => {
                let text =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
                result.structured_content = Some(value);
                Ok(result.into())
            }
            // Tool-level failures must reach the caller as tool results
            // (is_error=true), not protocol errors.
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into()),
        }
    }
}

/// Serve the MCP server over stdio until the client disconnects.
pub async fn serve(default_browser: Browser) -> Result<(), BmError> {
    let server = BmMcpServer::new(default_browser);
    let transport = rmcp::transport::stdio();
    let running = server
        .serve(transport)
        .await
        .map_err(|e| BmError::Other(format!("mcp server failed to start: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| BmError::Other(format!("mcp server stopped abnormally: {e}")))?;
    Ok(())
}
