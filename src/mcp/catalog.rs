use std::sync::Arc;
use rmcp::model::{JsonObject, Tool};
use serde_json::{json, Value};

/// Build an input schema: object type with the given properties and
/// required keys, closed to additional properties.
pub(crate) fn schema(properties: Value, required: &[&str]) -> Arc<JsonObject> {
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

/// The tool catalog: name, model-facing description, and a precise JSON
/// Schema for the input. Schemas are hand-written here (the model-facing
/// contract) — keep them in sync with the handlers in `dispatcher.rs`.
pub fn tool_definitions() -> Vec<Tool> {
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
        Tool::new("export_session", "Export session cookies strictly scoped to a concrete domain (wildcards prohibited). Sensitive values are masked by default unless allow_raw=true is explicitly requested", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to export cookies from"},
            "domain": {"type": "string", "description": "Target domain (mandatory, wildcards like '*' prohibited, e.g. 'github.com')"},
            "browser": browser_enum,
            "allow_raw": {"type": "boolean", "description": "Explicitly allow unmasked plaintext cookie values (requires security authorization)"}
        }}), &["name", "domain"])),
        Tool::new("group_list", "List all multi-profile session groups/workspaces with member profiles and running statuses", schema(json!({"properties": {}}), &[])),
        Tool::new("group_open", "Launch all browser profiles in a workspace group side-by-side with isolated data dirs, proxies, and optional CDP endpoints", schema(json!({"properties": {
            "name": {"type": "string", "description": "Name of the group to launch"},
            "cdp": {"type": "boolean", "description": "Enable CDP with isolated OS-assigned ports for launched profiles"},
            "urls": {"type": "array", "items": {"type": "string"}, "description": "Extra URLs to open in every launched profile"}
        }}), &["name"])),
        Tool::new("group_close", "Gracefully close all running browser profiles in a workspace group", schema(json!({"properties": {
            "name": {"type": "string", "description": "Name of the group to close"},
            "force": {"type": "boolean", "description": "Force kill browser instances if graceful close times out"}
        }}), &["name"])),
        Tool::new("group_create", "Create a new multi-profile session group/workspace", schema(json!({"properties": {
            "name": {"type": "string", "description": "Group name (alphanumeric, dash, underscore)"},
            "description": {"type": "string", "description": "Optional description for the group"},
            "members": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional list of initial member profiles in format 'profile' or 'browser:profile'"
            }
        }}), &["name"])),
        Tool::new("group_delete", "Delete a multi-profile session group/workspace (underlying profiles remain intact)", schema(json!({"properties": {
            "name": {"type": "string", "description": "Name of the group to delete"}
        }}), &["name"])),
        Tool::new("lease_acquire", "Acquire an exclusive lease lock on a profile for an agent to prevent concurrent modifications or destructive actions", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to lock"},
            "holder": {"type": "string", "description": "Agent identifier or caller identity"},
            "browser": browser_enum,
            "ttl": {"type": "integer", "minimum": 1, "description": "Lease duration in seconds (defaults to 300)"},
            "purpose": {"type": "string", "description": "Task description or reason for lease"},
            "force": {"type": "boolean", "description": "Break any existing active lease held by another agent"}
        }}), &["name", "holder"])),
        Tool::new("lease_release", "Release an agent's active lease lock on a browser profile", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile to unlock"},
            "holder": {"type": "string", "description": "Agent identifier who currently holds the lease"},
            "browser": browser_enum,
            "force": {"type": "boolean", "description": "Force release even if holder does not match"}
        }}), &["name", "holder"])),
        Tool::new("lease_renew", "Renew or extend the TTL of an active lease lock held by an agent", schema(json!({"properties": {
            "name": {"type": "string", "description": "Profile whose lease to renew"},
            "holder": {"type": "string", "description": "Agent identifier holding the lease"},
            "browser": browser_enum,
            "ttl": {"type": "integer", "minimum": 1, "description": "New lease duration in seconds from now (defaults to 300)"}
        }}), &["name", "holder"])),
        Tool::new("lease_list", "List all active agent leases across profiles for a browser", schema(json!({"properties": {
            "browser": browser_enum
        }}), &[])),
    ]
}
