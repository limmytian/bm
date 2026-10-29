//! Integration tests for `bm mcp` over HTTP/SSE transport.

use std::time::Duration;

use bm::browser::Browser;
use bm::mcp::http::{serve_http, HttpTransportConfig};
use bm::mcp::BmMcpServer;

fn get_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr").port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_http_health_endpoint_and_token_auth() {
    let port = get_free_port();
    let token = "super-secret-token-42".to_string();

    let config = HttpTransportConfig {
        host: "127.0.0.1".into(),
        port,
        token: Some(token.clone()),
        cors_origin: None,
    };

    let server = BmMcpServer::new(Browser::Chrome);

    // Spawn server in background
    let server_handle = tokio::spawn(async move {
        let _ = serve_http(server, config).await;
    });

    // Wait briefly for server startup
    tokio::time::sleep(Duration::from_millis(200)).await;

    let base_url = format!("http://127.0.0.1:{port}");
    let token_clone = token.clone();

    // Run synchronous ureq calls on a blocking thread
    tokio::task::spawn_blocking(move || {
        // 1. Request without token -> 401 Unauthorized
        let resp = ureq::get(&format!("{base_url}/health")).call();
        assert!(resp.is_err(), "request without token must be rejected");
        if let Err(ureq::Error::Status(code, _)) = resp {
            assert_eq!(code, 401);
        }

        // 2. Request with invalid token -> 401 Unauthorized
        let resp_bad = ureq::get(&format!("{base_url}/health"))
            .set("Authorization", "Bearer invalid-token")
            .call();
        assert!(resp_bad.is_err());
        if let Err(ureq::Error::Status(code, _)) = resp_bad {
            assert_eq!(code, 401);
        }

        // 3. Request with valid Bearer token header -> 200 OK
        let resp_ok = ureq::get(&format!("{base_url}/health"))
            .set("Authorization", &format!("Bearer {token_clone}"))
            .call()
            .expect("valid token header must succeed");
        assert_eq!(resp_ok.status(), 200);
        let body_str = resp_ok.into_string().expect("response string");
        let body: serde_json::Value = serde_json::from_str(&body_str).expect("json response");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "bm-mcp");

        // 4. Request with valid query parameter token -> 200 OK
        let resp_query = ureq::get(&format!("{base_url}/health?token={token_clone}"))
            .call()
            .expect("valid query token must succeed");
        assert_eq!(resp_query.status(), 200);
    })
    .await
    .unwrap();

    server_handle.abort();
}

#[tokio::test]
async fn test_non_loopback_binding_without_token_is_rejected() {
    let port = get_free_port();
    let config = HttpTransportConfig {
        host: "0.0.0.0".into(),
        port,
        token: None, // No token specified for public host
        cors_origin: None,
    };

    let server = BmMcpServer::new(Browser::Chrome);
    let res = serve_http(server, config).await;
    assert!(res.is_err(), "non-loopback binding without token must fail");
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("Security violation"),
        "error should explain security violation: {err_msg}"
    );
}
