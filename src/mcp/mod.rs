//! Built-in Model Context Protocol (MCP) server for bm.
//!
//! Exposes browser management capabilities to AI agents over standard MCP transports.

pub mod catalog;
pub mod dispatcher;
pub mod stdio;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::ErrorData as McpError;
use rmcp::{RoleServer, ServerHandler};
use serde_json::Value;

use crate::browser::Browser;
use crate::error::BmError;

#[derive(Clone)]
pub struct BmMcpServer {
    default_browser: Browser,
}

impl BmMcpServer {
    pub fn new(default_browser: Browser) -> Self {
        Self { default_browser }
    }

    #[allow(dead_code)]
    pub fn default_browser(&self) -> Browser {
        self.default_browser
    }
}

impl ServerHandler for BmMcpServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "bm manages Chromium-family browser profiles (Chrome, Edge, Brave, Chromium) with \
             isolated user-data-dirs, per-profile egress proxies, workspaces, agent leases, and local snapshots. \
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
        Ok(ListToolsResult::with_all_items(catalog::tool_definitions()))
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
        match dispatcher::dispatch_tool(self.default_browser, &request.name, args).await {
            Ok(value) => {
                let text =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
                result.structured_content = Some(value);
                Ok(result.into())
            }
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into()),
        }
    }
}

/// Serve the MCP server over stdio until the client disconnects.
pub async fn serve(default_browser: Browser) -> Result<(), BmError> {
    let server = BmMcpServer::new(default_browser);
    stdio::serve_stdio(server).await
}
