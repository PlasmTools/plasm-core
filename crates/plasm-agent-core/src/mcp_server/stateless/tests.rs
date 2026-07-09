//! SEP-2575 stateless HTTP integration tests.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::mcp_stream_auth::PLASM_MCP_ANONYMOUS_BEARER_TOKEN;
use crate::test_support::operation_fixtures::minimal_host;

fn valid_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "stateless-test",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

async fn post_mcp(app: Router, method: &str, params: Value, id: i64) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let auth = format!("Bearer {}", PLASM_MCP_ANONYMOUS_BEARER_TOKEN);
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method)
        .header("accept", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    (status, data)
}

#[tokio::test]
async fn stateless_discover_returns_capabilities() {
    let app = super::router(minimal_host()).await;
    let (status, data) = post_mcp(
        app,
        "server/discover",
        json!({ "_meta": valid_meta() }),
        1,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(data["result"]["supportedVersions"]
        .as_array()
        .is_some_and(|a| !a.is_empty()));
    assert_eq!(data["result"]["serverInfo"]["name"], "plasm");
    assert!(data["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn stateless_rejects_missing_meta_with_400() {
    let app = super::router(minimal_host()).await;
    let (status, data) = post_mcp(app, "server/discover", json!({}), 101).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(data["error"]["code"], -32602);
}

#[tokio::test]
async fn stateless_initialize_returns_404() {
    let app = super::router(minimal_host()).await;
    let (status, data) = post_mcp(
        app,
        "initialize",
        json!({ "_meta": valid_meta() }),
        500,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(data["error"]["code"], -32601);
}

#[tokio::test]
async fn stateless_tools_list_with_valid_meta() {
    let app = super::router(minimal_host()).await;
    let (status, data) = post_mcp(
        app,
        "tools/list",
        json!({ "_meta": valid_meta() }),
        202,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(data["result"]["tools"].as_array().is_some());
}

#[tokio::test]
async fn stateless_header_mismatch_returns_32020() {
    let app = super::router(minimal_host()).await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 302,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "v999.0.0",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "stateless-test",
                    "version": "1.0.0"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    let auth = format!("Bearer {}", PLASM_MCP_ANONYMOUS_BEARER_TOKEN);
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "server/discover")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["error"]["code"], -32020);
}
