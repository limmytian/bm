//! Streamable HTTP / SSE transport for `bm mcp`.
//!
//! Exposes an MCP-compliant HTTP endpoint with Server-Sent Events (SSE) support,
//! session management, Bearer token authentication, and DNS rebinding protections.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, Request};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};

use crate::error::BmError;
use super::BmMcpServer;

/// Configuration options for launching the MCP HTTP server.
#[derive(Debug, Clone)]
pub struct HttpTransportConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub cors_origin: Option<String>,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Middleware to enforce Bearer token authentication if configured.
async fn auth_middleware(
    required_token: Option<String>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(ref secret) = required_token {
        let auth_header = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        let bearer_valid = auth_header
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|t| t == secret)
            .unwrap_or(false);

        let query_valid = query.token.as_ref().map(|t| t == secret).unwrap_or(false);

        if !bearer_valid && !query_valid {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "Unauthorized: valid Bearer token or ?token= query parameter required"
                })),
            )
                .into_response();
        }
    }

    next.run(request).await
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "bm-mcp",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Serve the MCP server over HTTP/SSE until cancelled or interrupted.
pub async fn serve_http(
    server: BmMcpServer,
    config: HttpTransportConfig,
) -> Result<(), BmError> {
    // Security check: non-loopback binding requires mandatory token authentication.
    let is_loopback = config.host == "127.0.0.1" || config.host == "localhost" || config.host == "::1";
    if !is_loopback && config.token.is_none() {
        return Err(BmError::Other(
            "Security violation: binding to non-loopback address without an authentication token is prohibited. \
             Provide --token or set BM_AUTH_TOKEN."
                .into(),
        ));
    }

    let default_browser = server.default_browser();
    let cancel_token = CancellationToken::new();

    // Configure rmcp StreamableHttpServer
    let mut http_config = StreamableHttpServerConfig::default();
    http_config.sse_keep_alive = Some(Duration::from_secs(15));
    http_config.sse_retry = Some(Duration::from_secs(3));
    http_config.cancellation_token = cancel_token.clone();

    // Allowed hosts for DNS rebinding protection
    if is_loopback {
        http_config.allowed_hosts = vec![
            "localhost".into(),
            "127.0.0.1".into(),
            format!("127.0.0.1:{}", config.port),
            format!("localhost:{}", config.port),
        ];
    } else {
        http_config.allowed_hosts = vec![
            config.host.clone(),
            format!("{}:{}", config.host, config.port),
            "localhost".into(),
            "127.0.0.1".into(),
        ];
    }

    let session_manager = Arc::new(LocalSessionManager::default());
    let mcp_service = StreamableHttpService::new(
        move || Ok(BmMcpServer::new(default_browser)),
        session_manager,
        http_config,
    );

    // Build Axum router
    let mut app = Router::new()
        .route("/health", get(health_handler))
        .fallback_service(mcp_service);

    // Apply CORS
    if let Some(origin) = config.cors_origin {
        if origin == "*" {
            app = app.layer(CorsLayer::new().allow_origin(Any).allow_headers(Any));
        } else if let Ok(header_val) = origin.parse() {
            app = app.layer(
                CorsLayer::new()
                    .allow_origin(vec![header_val])
                    .allow_headers(Any),
            );
        }
    }

    // Apply Token auth middleware
    let token_clone = config.token.clone();
    app = app.layer(middleware::from_fn(move |headers, query, req, next| {
        auth_middleware(token_clone.clone(), headers, query, req, next)
    }));

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(|e| BmError::Other(format!("invalid listen address: {e}")))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| BmError::Other(format!("failed to bind TCP listener on {addr}: {e}")))?;

    let actual_addr = listener
        .local_addr()
        .map_err(|e| BmError::Other(format!("failed to get local addr: {e}")))?;

    eprintln!(
        "bm MCP server running on http://{} (SSE endpoint: /sse, RPC: /messages)",
        actual_addr
    );

    // Run axum with graceful shutdown on Ctrl+C / SIGTERM
    let cancel_token_clone = cancel_token.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("\nShutting down bm MCP HTTP server...");
            cancel_token_clone.cancel();
        })
        .await
        .map_err(|e| BmError::Other(format!("HTTP server terminated with error: {e}")))?;

    Ok(())
}
