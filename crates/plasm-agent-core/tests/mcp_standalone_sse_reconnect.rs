//! Regression: GET SSE reconnect must supersede the prior standalone stream (no HTTP 409).

use std::sync::Arc;
use std::time::Duration;

use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::mcp_server::run_mcp_server;
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

fn test_host() -> plasm_agent_core::server_state::PlasmHostState {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        "langmatrix".into(),
        "Lang Matrix".into(),
        vec!["matrix".into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

async fn mcp_get_sse_status(
    client: &reqwest::Client,
    base: &str,
    session: &str,
) -> reqwest::StatusCode {
    client
        .get(base)
        .header("accept", "text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .header("MCP-Session-Id", session)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("GET SSE")
        .status()
}

#[tokio::test]
async fn standalone_sse_reconnect_supersedes_prior_listener() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let plasm = Arc::new(test_host());
    tokio::spawn(async move {
        let _ = run_mcp_server("127.0.0.1", port, plasm).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let base = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::new();
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    });

    let init = client
        .post(&base)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&init_body)
        .send()
        .await
        .expect("initialize");
    assert!(init.status().is_success(), "initialize: {}", init.status());
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("MCP-Session-Id")
        .to_string();

    assert_eq!(
        mcp_get_sse_status(&client, &base, &session).await,
        reqwest::StatusCode::OK,
        "first GET SSE"
    );
    assert_eq!(
        mcp_get_sse_status(&client, &base, &session).await,
        reqwest::StatusCode::OK,
        "reconnect GET SSE must not 409"
    );
}
