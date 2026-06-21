//! HTTP-only contracts for terminal extensions not covered by `plasm` CLI insta tests
//! (`crates/plasm/tests/plasm_cli_server_insta.rs`): `/symbols`, `/status`, `/runs`, and
//! `POST /execute/.../context`. Discovery TSV shape and plan/run flows are CLI snapshots.

use axum::body::Body;
use axum::extract::Extension;
use axum::http::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use axum::http::{Request, StatusCode};
use axum::Router;
use plasm_agent_core::http::{build_plasm_host_state, health_public_routes, PlasmHostBootstrap};
use plasm_agent_core::http_discovery::discovery_routes_protected;
use plasm_agent_core::http_execute::execute_routes;
use plasm_agent_core::incoming_auth::IncomingPrincipal;
use plasm_agent_core::server_state::PlasmHostState;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;

fn test_state() -> PlasmHostState {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        "overshow".into(),
        "Overshow".into(),
        vec!["demo".into()],
        cgs.clone(),
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: plasm_agent_core::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

fn test_app(st: PlasmHostState) -> Router<()> {
    Router::new()
        .merge(health_public_routes())
        .merge(
            Router::new()
                .merge(discovery_routes_protected())
                .merge(execute_routes())
                .layer(axum::middleware::from_fn(
                    plasm_agent_core::incoming_auth::incoming_auth_http_middleware,
                )),
        )
        .layer(Extension(st))
        .layer(Extension(IncomingPrincipal(None)))
}

async fn open_overshow_profile_session(app: &Router<()>) -> (String, String) {
    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res.headers().get(LOCATION).unwrap().to_str().unwrap();
    let get = Request::builder()
        .method("GET")
        .uri(loc)
        .header(ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(get).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&b).unwrap();
    (
        created["prompt_hash"].as_str().unwrap().to_string(),
        created["session"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn execute_symbols_status_and_runs() {
    let app = test_app(test_state());
    let (ph, sid) = open_overshow_profile_session(&app).await;

    let sym = Request::builder()
        .method("GET")
        .uri(format!("/execute/{ph}/{sid}/symbols"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(sym).await.unwrap().status(),
        StatusCode::OK
    );

    let stat = Request::builder()
        .method("GET")
        .uri(format!("/execute/{ph}/{sid}/status"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(stat).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let stbody: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stbody["alive"], true);

    let runs = Request::builder()
        .method("GET")
        .uri(format!("/execute/{ph}/{sid}/runs"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(runs).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn execute_context_expand_http() {
    let app = test_app(test_state());
    let (ph, sid) = open_overshow_profile_session(&app).await;

    let ctx = Request::builder()
        .method("POST")
        .uri(format!("/execute/{ph}/{sid}/context"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "seeds": [{ "api": "overshow", "entity": "RecordedContent" }]
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(ctx).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "context expand should succeed for second entity in same catalog"
    );
}
