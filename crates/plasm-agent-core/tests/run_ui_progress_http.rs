//! Public Run Explorer progress routes: auth-free access, JSON poll, CORS.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Extension;
use axum::Router;
use plasm_agent_core::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use plasm_agent_core::execute_session::{ExecuteSession, SessionReuseKey};
use plasm_agent_core::http::{build_plasm_host_state, health_public_routes, PlasmHostBootstrap};
use plasm_agent_core::mcp_logical_ref::parse_logical_session_wire_ref;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::CGS;
use plasm_runtime::{ExecutionEngine, ExecutionMode};
use tower::ServiceExt;

fn progress_app(st: plasm_agent_core::server_state::PlasmHostState) -> Router<()> {
    health_public_routes().layer(Extension(st))
}

async fn seeded_host_with_running_op() -> (plasm_agent_core::server_state::PlasmHostState, String) {
    let cgs = Arc::new(CGS::new());
    let st = build_plasm_host_state(PlasmHostBootstrap {
        engine: ExecutionEngine::new(Default::default()).expect("engine"),
        mode: ExecutionMode::Live,
        registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
            "default".into(),
            "Default".into(),
            vec!["default".into()],
            cgs.clone(),
        )])),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });

    let ref_str = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let logical_id = parse_logical_session_wire_ref(ref_str).expect("logical ref");
    let ph = PromptHashHex::from_prompt_sha256("run-ui-progress-http-test").to_string();
    let sid = ExecuteSessionId::new_random().to_string();
    let sess = ExecuteSession::new(
        ph.clone(),
        "p".into(),
        cgs.clone(),
        indexmap::IndexMap::new(),
        "default".into(),
        String::new(),
        String::new(),
        None,
        vec!["Pet".into()],
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    );
    let handle = sess.mint_operation_handle(ref_str);
    sess.try_begin_async_operation(
        handle,
        plasm_runtime::CancelSignal::new(),
        plasm_agent_core::operation::OpAcceptContext::default(),
    )
    .expect("begin op");

    let reuse_key = SessionReuseKey {
        tenant_scope: String::new(),
        entry_id: "default".into(),
        catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        entities: vec!["Pet".into()],
        context_intent: None,
        ranked_capabilities: None,
        principal: None,
        logical_session_id: Some(logical_id.as_uuid().to_string()),
    };

    st.sessions
        .insert(reuse_key, ph.clone(), sid.clone(), sess)
        .await;
    st.logical_execute_bindings
        .insert(logical_id.as_uuid(), ph.clone(), sid.clone())
        .await;

    (st, ref_str.to_string())
}

#[tokio::test]
async fn progress_poll_404_without_running_op() {
    let (st, ref_str) = seeded_host_with_running_op().await;
    let app = progress_app(st);
    let uri = format!("/v1/run/ui/progress/{ref_str}?plan_commit_ref=pc99");
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn progress_poll_200_with_line_while_running() {
    let (st, ref_str) = seeded_host_with_running_op().await;
    let app = progress_app(st);
    let uri = format!("/v1/run/ui/progress/{ref_str}");
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("line").and_then(|v| v.as_str()).is_some());
}

#[tokio::test]
async fn progress_cors_mirrors_foreign_origin() {
    let (st, ref_str) = seeded_host_with_running_op().await;
    let app = progress_app(st);
    let uri = format!("/v1/run/ui/progress/{ref_str}");
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::ORIGIN, "https://cursor.sh")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("https://cursor.sh")
    );
}
