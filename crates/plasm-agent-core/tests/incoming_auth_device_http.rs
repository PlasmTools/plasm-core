//! HTTP integration for CLI device login (`/v1/incoming-auth/device/*`).

use std::sync::Arc;

use auth_framework::storage::core::AuthStorage;
use auth_framework::storage::MemoryStorage;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::incoming_auth::{IncomingAuthConfig, IncomingAuthMode, IncomingAuthVerifier};
use plasm_agent_core::incoming_auth_device::{
    incoming_auth_device_public_routes, mint_incoming_access_token,
};
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use serde_json::json;
use std::path::Path;
use tower::ServiceExt;

const TEST_SECRET: &str = "device-http-test-secret-01234567890123456789012";

fn fixture_registry() -> Arc<InMemoryCgsRegistry> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = Arc::new(load_schema(&dir).expect("prompt matrix schema"));
    Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        "prompt_matrix".into(),
        "Prompt matrix".into(),
        vec![],
        cgs,
    )]))
}

fn test_host_state(storage: Arc<MemoryStorage>) -> plasm_agent_core::server_state::PlasmHostState {
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("execution engine");
    let incoming = IncomingAuthVerifier::new(IncomingAuthConfig {
        mode: IncomingAuthMode::Optional,
        jwt_secret: Some(TEST_SECRET.to_string()),
        jwt_issuer: None,
        jwt_audience: None,
        api_keys_file: None,
    })
    .expect("incoming auth");
    let mut st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: fixture_registry(),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: Some(Arc::new(incoming)),
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    st.oss.auth_storage = Some(storage);
    st
}

#[tokio::test]
async fn device_start_requires_auth_storage() {
    let mut st = test_host_state(Arc::new(MemoryStorage::new()));
    st.oss.auth_storage = None;
    let app = incoming_auth_device_public_routes().layer(axum::Extension(st));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/incoming-auth/device/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn device_start_and_poll_approved_via_kv() {
    let storage = Arc::new(MemoryStorage::new());
    let st = test_host_state(storage.clone());
    let verifier = st.incoming_auth.as_ref().unwrap().clone();
    let app = incoming_auth_device_public_routes().layer(axum::Extension(st));

    let start_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/incoming-auth/device/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::OK);

    let start_body = axum::body::to_bytes(start_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_json: serde_json::Value = serde_json::from_slice(&start_body).unwrap();
    let device_code = start_json["device_code"].as_str().unwrap();
    let user_code = start_json["user_code"].as_str().unwrap();
    assert!(start_json["verification_uri"]
        .as_str()
        .unwrap()
        .contains("/device?user_code="));

    let token = mint_incoming_access_token(&verifier, "github:99", "tenant-device").unwrap();

    let sess = json!({
        "user_code": user_code,
        "expires_at_unix": u64::MAX / 2,
        "poll_interval_secs": 5,
        "status": { "approved": { "access_token": token } }
    });
    storage
        .store_kv(
            &format!("plasm:incoming_auth_device:v1:code:{device_code}"),
            sess.to_string().as_bytes(),
            None,
        )
        .await
        .unwrap();

    let poll_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/incoming-auth/device/poll")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "device_code": device_code }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll_resp.status(), StatusCode::OK);
    let poll_body = axum::body::to_bytes(poll_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let poll_json: serde_json::Value = serde_json::from_slice(&poll_body).unwrap();
    assert!(poll_json.get("access_token").is_some());
}
