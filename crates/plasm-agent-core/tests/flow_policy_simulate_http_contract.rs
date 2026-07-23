//! HTTP contract for `/internal/flow-policy/v1/simulate`.
//!
//! Ephemeral draft arm needs no flow-policy repository. Stored draft/published
//! arms fail closed with typed codes when the repository is absent.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_flow_policy::flow_policy_routes;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use serde_json::{json, Value};
use tower::ServiceExt;

const DEV_SECRET: &str = "dev-plasm-mcp-control-plane-secret-32chars-min!!";

fn matrix_host() -> plasm_agent_core::server_state::PlasmHostState {
    if std::env::var_os("PLASM_HTTP_NO_SYSTEM_PROXY").is_none() {
        // SAFETY: test-only process env before reqwest Client build.
        unsafe { std::env::set_var("PLASM_HTTP_NO_SYSTEM_PROXY", "1") };
    }
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
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
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

fn app() -> Router {
    flow_policy_routes().layer(Extension(matrix_host()))
}

async fn post_simulate(body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/internal/flow-policy/v1/simulate")
        .header("content-type", "application/json")
        .header("x-plasm-control-plane-secret", DEV_SECRET)
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    (status, json)
}

#[tokio::test]
async fn simulate_http_happy_ephemeral_deny() {
    let (status, body) = post_simulate(json!({
        "tenant_id": "t1",
        "workspace_slug": "ws",
        "project_slug": "proj",
        "policy_arm": "draft",
        "seeds": [{"api": "langmatrix", "entity": "LangItem"}],
        "program": "LangItem(\"i2\").delete()",
        "intent": "http contract happy",
        "policy": {
            "default_posture": "allow",
            "forbidden": [],
            "capability_gates": [{
                "pattern": {"capability": "delete", "entity": "LangItem"},
                "enforcement": "deny"
            }],
            "sanitizers": []
        }
    }))
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["dry_verdict"], "deny");
    assert!(body.get("comp").is_some());
    assert!(body.get("plan_ux_reflection").is_some());
}

#[tokio::test]
async fn simulate_http_draft_missing_without_ephemeral() {
    let (status, body) = post_simulate(json!({
        "tenant_id": "t1",
        "workspace_slug": "ws",
        "project_slug": "proj",
        "policy_arm": "draft",
        "seeds": [{"api": "langmatrix", "entity": "LangItem"}],
        "program": "LangItem(\"i2\")",
        "intent": "http contract draft_missing"
    }))
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(body["code"], "draft_missing");
    assert!(body["error"].as_str().unwrap_or("").contains("draft"));
}

#[tokio::test]
async fn simulate_http_compile_failed() {
    let (status, body) = post_simulate(json!({
        "tenant_id": "t1",
        "workspace_slug": "ws",
        "project_slug": "proj",
        "policy_arm": "draft",
        "seeds": [{"api": "langmatrix", "entity": "LangItem"}],
        "program": "this is not valid plasm (((",
        "intent": "http contract compile_failed",
        "policy": {
            "default_posture": "allow",
            "forbidden": [],
            "capability_gates": [],
            "sanitizers": []
        }
    }))
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(body["code"], "compile_failed");
}
