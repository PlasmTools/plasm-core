//! Fast oneshot HTTP smoke tests for long-operation query params and dispatch wiring.
#![recursion_limit = "256"]

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
use plasm_agent_core::mcp_transport_store::{
    descriptor_from_operation_state, ExecuteSessionRegistry, OperationPersistPatch,
};
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::CgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use tower::ServiceExt;

/// Bare list queries are host-page-bounded (`ok`); aggregate still requires plan review.
fn matrix_program(entity: &str, operations: &str) -> String {
    format!("class MatrixOperation(Program):\n    def build(self):\n        return {entity}.query(){operations}\n")
}

fn langmatrix_host_state() -> plasm_agent_core::server_state::PlasmHostState {
    langmatrix_host_with_registry(
        ExecuteSessionRegistry::default(),
        Arc::new(RunArtifactStore::memory()),
    )
}

fn langmatrix_host_with_registry(
    execute_session_registry: ExecuteSessionRegistry,
    run_artifacts: Arc<RunArtifactStore>,
) -> plasm_agent_core::server_state::PlasmHostState {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let reg = CgsRegistry::from_pairs(vec![(
        "langmatrix".into(),
        "Lang Matrix".into(),
        vec!["matrix".into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    let mut st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts,
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .expect("valid catalog fixture");
    st.oss.execute_session_registry = execute_session_registry;
    st
}

fn test_app(st: plasm_agent_core::server_state::PlasmHostState) -> Router<()> {
    execute_routes()
        .layer(Extension(st))
        .layer(Extension(IncomingPrincipal(None)))
}

async fn open_langitem_session(
    app: &Router<()>,
    st: &plasm_agent_core::server_state::PlasmHostState,
) -> (String, String, String) {
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
    let session = st
        .get_execute_session(&created.prompt_hash, &created.session)
        .await
        .expect("opened matrix session");
    let symbols =
        plasm_agent_core::plasm_plan_run::symbol_map_for_plasm_surface_parse(&session, None);
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    assert!(
        entity.starts_with('e'),
        "use served opaque symbol: {entity}"
    );
    (created.prompt_hash, created.session, entity)
}

#[tokio::test]
async fn plan_dry_run_mints_plan_commit_ref() {
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let uri = format!("/execute/{ph}/{sid}?mode=plan");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(matrix_program(
            &entity,
            ".aggregate(n=agg.count())",
        )))
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
        .and_then(|p| p.get("run_ref"))
        .and_then(|v| v.as_str())
        .expect("run_ref");
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
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let uri = format!("/execute/{ph}/{sid}");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(
            &entity,
            ".aggregate(n=agg.count())",
        )))
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
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, _entity) = open_langitem_session(&app, &st).await;
    let uri = format!("/execute/{ph}/{sid}");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from("wait(o999)"))
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={doc}");
    let failure: plasm_runtime::ExecutionFailure =
        serde_json::from_value(doc["failure"].clone()).expect("typed operation failure");
    assert_eq!(failure.cause, plasm_runtime::FailureCause::Program);
    assert_eq!(
        failure.recovery,
        plasm_runtime::RecoveryDisposition::RepairProgram
    );
    assert_eq!(
        failure.code,
        plasm_agent_core::operation_error::OperationError::CODE_UNKNOWN
    );
    assert!(failure
        .diagnostic()
        .contains("unknown operation handle `o999`"));
}

#[tokio::test]
async fn review_plan_auto_async_without_wait_false() {
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let plan_uri = format!("/execute/{ph}/{sid}?mode=plan");
    let plan_req = Request::builder()
        .method("POST")
        .uri(&plan_uri)
        .header("accept", "application/json")
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(matrix_program(
            &entity,
            ".aggregate(n=agg.count())",
        )))
        .unwrap();
    let plan_res = app.clone().oneshot(plan_req).await.unwrap();
    assert_eq!(plan_res.status(), StatusCode::OK);
    let plan_body = axum::body::to_bytes(plan_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let plan_doc: serde_json::Value = serde_json::from_slice(&plan_body).unwrap();
    assert_eq!(
        plan_doc
            .get("_meta")
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("dry_verdict"))
            .and_then(|v| v.as_str()),
        Some("review")
    );
    let pc = plan_doc
        .get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("run_ref"))
        .and_then(|v| v.as_str())
        .expect("run_ref");
    let live_uri = format!("/execute/{ph}/{sid}?wait=false&plan_commit_ref={pc}");
    let live_req = Request::builder()
        .method("POST")
        .uri(&live_uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(
            &entity,
            ".aggregate(n=agg.count())",
        )))
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
    let md = live_doc
        .get("run_markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(md.contains("`o"), "markdown: {md}");
}

#[tokio::test]
async fn parallel_async_live_runs_accept_distinct_handles() {
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let start_uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let first_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(
            &entity,
            ".page_size(1).take(10)",
        )))
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
        .body(Body::from(matrix_program(&entity, ".take(2)")))
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
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(&entity, ".take(2)")))
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
    assert!(md.contains("wait("), "accept should nudge wait poll: {md}");
}

#[tokio::test]
async fn wait_poll_unchanged_returns_compact_equals_line() {
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    let start_uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let start_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(&entity, ".take(2)")))
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

#[tokio::test]
async fn running_ops_cap_rejects_when_exceeded_http() {
    let prev = std::env::var("PLASM_MAX_RUNNING_OPS_PER_SESSION").ok();
    unsafe {
        std::env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", "2");
    }
    let st = langmatrix_host_state();
    let app = test_app(st.clone());
    let (ph, sid, entity) = open_langitem_session(&app, &st).await;
    for _ in 0..2 {
        let uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
        let req = Request::builder()
            .method("POST")
            .uri(&uri)
            .header("accept", "application/json")
            .body(Body::from(matrix_program(&entity, ".take(2)")))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "async accept under cap");
    }
    let uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(&entity, ".take(2)")))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("too_many_operations"),
        "expected cap error: {text}"
    );
    match prev {
        Some(v) => unsafe {
            std::env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", v);
        },
        None => unsafe {
            std::env::remove_var("PLASM_MAX_RUNNING_OPS_PER_SESSION");
        },
    }
}

#[tokio::test]
async fn cross_pod_wait_from_shared_session_registry() {
    let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
    let artifacts = Arc::new(RunArtifactStore::memory());
    let host_a = langmatrix_host_with_registry(execute_registry.clone(), artifacts.clone());
    let host_b = langmatrix_host_with_registry(execute_registry, artifacts);
    let app_a = test_app(host_a.clone());
    let app_b = test_app(host_b);
    let (ph, sid, entity) = open_langitem_session(&app_a, &host_a).await;

    let start_uri = format!("/execute/{ph}/{sid}?wait=false&force=true");
    let start_req = Request::builder()
        .method("POST")
        .uri(&start_uri)
        .header("accept", "application/json")
        .body(Body::from(matrix_program(&entity, ".take(2)")))
        .unwrap();
    let start_res = app_a.clone().oneshot(start_req).await.unwrap();
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
        .expect("handle")
        .to_string();

    let sess = host_a
        .get_execute_session(&ph, &sid)
        .await
        .expect("session");
    let op =
        sess.get_operation(&plasm_core::OperationHandle::parse(&handle).expect("parse handle"));
    if let Some(op) = op {
        host_a
            .execute_session_registry
            .patch_session_operations(
                &ph,
                &sid,
                OperationPersistPatch::Upsert(Box::new(descriptor_from_operation_state(
                    &plasm_core::OperationHandle::parse(&handle).unwrap(),
                    &op,
                    1_700_000_000,
                ))),
            )
            .await;
    }
    host_a.sessions.purge_all().await;

    let wait_uri = format!("/execute/{ph}/{sid}");
    let wait_req = Request::builder()
        .method("POST")
        .uri(&wait_uri)
        .header("accept", "application/json")
        .body(Body::from(format!("wait({handle})")))
        .unwrap();
    let wait_res = app_b.oneshot(wait_req).await.unwrap();
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
        !md.contains("unknown operation handle"),
        "cross-pod wait should resolve persisted handle: {md}"
    );
}
