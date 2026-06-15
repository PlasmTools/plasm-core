//! Fast oneshot HTTP smoke tests for long-operation query params and dispatch wiring.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header::CONTENT_TYPE, header::LOCATION, Request, StatusCode};
use axum::Extension;
use axum::Router;
use plasm_agent_core::execute_path_ids::PromptHashHex;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::{execute_routes, CreateExecuteSessionResponse};
use plasm_agent_core::incoming_auth::IncomingPrincipal;
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use tower::ServiceExt;

fn langmatrix_host_state() -> plasm_agent_core::server_state::PlasmHostState {
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
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

fn test_app(st: plasm_agent_core::server_state::PlasmHostState) -> Router<()> {
    execute_routes()
        .layer(Extension(st))
        .layer(Extension(IncomingPrincipal(None)))
}

async fn open_langitem_session(app: &Router<()>) -> (String, String) {
    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "langmatrix", "entities": ["LangItem"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res
        .headers()
        .get(LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let get = Request::builder()
        .method("GET")
        .uri(&loc)
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(get).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: CreateExecuteSessionResponse = serde_json::from_slice(&body).unwrap();
    let expected_hash = PromptHashHex::from_prompt_sha256(&created.prompt);
    assert_eq!(created.prompt_hash, expected_hash.to_string());
    (created.prompt_hash, created.session)
}

#[tokio::test]
async fn plan_dry_run_mints_plan_commit_ref() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let uri = format!("/execute/{ph}/{sid}?mode=plan");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from("LangItem"))
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let pc = doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("plan_commit_ref"))
        .and_then(|v| v.as_str())
        .expect("plan_commit_ref");
    assert!(pc.starts_with("pc"));
    assert_eq!(
        doc.get("_meta")
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("dry_verdict"))
            .and_then(|v| v.as_str()),
        Some("review")
    );
}

#[tokio::test]
async fn live_blocked_without_force_returns_plan_requires_review() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let uri = format!("/execute/{ph}/{sid}");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem"))
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let detail = doc.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    assert!(detail.contains("plan_requires_review"), "detail: {detail}");
}

#[tokio::test]
async fn wait_unknown_handle_is_400() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let uri = format!("/execute/{ph}/{sid}");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from("wait(o999)"))
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let detail = doc.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    assert!(
        detail.contains("unknown operation handle"),
        "detail: {detail}"
    );
}

#[tokio::test]
async fn review_plan_auto_async_without_wait_false() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let plan_uri = format!("/execute/{ph}/{sid}?mode=plan");
    let plan_req = Request::builder()
        .method("POST")
        .uri(&plan_uri)
        .header("accept", "application/json")
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from("LangItem"))
        .unwrap();
    let plan_res = app.clone().oneshot(plan_req).await.unwrap();
    assert_eq!(plan_res.status(), StatusCode::OK);
    let plan_body = axum::body::to_bytes(plan_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let plan_doc: serde_json::Value = serde_json::from_slice(&plan_body).unwrap();
    let pc = plan_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("plan_commit_ref"))
        .and_then(|v| v.as_str())
        .expect("plan_commit_ref");
    let live_uri = format!("/execute/{ph}/{sid}?plan_commit_ref={pc}");
    let live_req = Request::builder()
        .method("POST")
        .uri(&live_uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem"))
        .unwrap();
    let live_res = app.oneshot(live_req).await.unwrap();
    assert_eq!(live_res.status(), StatusCode::OK);
    let live_body = axum::body::to_bytes(live_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let live_doc: serde_json::Value = serde_json::from_slice(&live_body).unwrap();
    assert_eq!(
        live_doc.get("operation").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        live_doc
            .get("_meta")
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("auto_async"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    let md = live_doc
        .get("run_markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(md.contains("`o"), "markdown: {md}");
}

#[tokio::test]
async fn parallel_async_live_runs_accept_distinct_handles() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let start_uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let first_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem.page_size(1).limit(10)"))
        .unwrap();
    let first_res = app.clone().oneshot(first_req).await.unwrap();
    assert_eq!(first_res.status(), StatusCode::OK);
    let first_body = axum::body::to_bytes(first_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_doc: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    let handle1 = first_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("continuity"))
        .and_then(|c| c.get("h"))
        .and_then(|v| v.as_str())
        .expect("operation handle");

    let second_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem.limit(2)"))
        .unwrap();
    let second_res = app.oneshot(second_req).await.unwrap();
    assert_eq!(second_res.status(), StatusCode::OK, "parallel async accept");
    let second_body = axum::body::to_bytes(second_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let second_doc: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    let handle2 = second_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("continuity"))
        .and_then(|c| c.get("h"))
        .and_then(|v| v.as_str())
        .expect("second operation handle");
    assert_ne!(handle1, handle2);
}

#[tokio::test]
async fn wait_false_async_accept_returns_operation_json() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem.limit(2)"))
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "expected json operation accept, got {ct}"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(doc.get("operation").and_then(|v| v.as_bool()), Some(true));
    let md = doc
        .get("run_markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(md.contains("`o"), "markdown: {md}");
    assert!(md.contains('+'), "compact accept: {md}");
    assert!(
        md.contains("wait("),
        "accept should nudge wait poll: {md}"
    );
}

#[tokio::test]
async fn wait_poll_unchanged_returns_compact_equals_line() {
    let app = test_app(langmatrix_host_state());
    let (ph, sid) = open_langitem_session(&app).await;
    let start_uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let start_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from("LangItem.limit(2)"))
        .unwrap();
    let start_res = app.clone().oneshot(start_req).await.unwrap();
    assert_eq!(start_res.status(), StatusCode::OK);
    let start_body = axum::body::to_bytes(start_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_doc: serde_json::Value = serde_json::from_slice(&start_body).unwrap();
    let handle = start_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("continuity"))
        .and_then(|c| c.get("h"))
        .and_then(|v| v.as_str())
        .expect("handle");
    let wait_uri = format!("/execute/{ph}/{sid}");
    let wait_req = Request::builder()
        .method("POST")
        .uri(&wait_uri)
        .header("accept", "application/json")
        .body(Body::from(format!("wait({handle})")))
        .unwrap();
    let wait_res = app.clone().oneshot(wait_req).await.unwrap();
    assert_eq!(wait_res.status(), StatusCode::OK);
    let wait_body = axum::body::to_bytes(wait_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let wait_doc: serde_json::Value = serde_json::from_slice(&wait_body).unwrap();
    let md = wait_doc
        .get("run_markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        md.contains('`') && (md.contains('=') || md.contains('~')),
        "progress line: {md}"
    );
    assert!(!md.contains("Poll:"), "no poll instructions on wait: {md}");
    if let Some(op) = wait_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("op"))
    {
        assert!(op.get("n").is_some(), "short-key op meta: {op}");
    }
}
