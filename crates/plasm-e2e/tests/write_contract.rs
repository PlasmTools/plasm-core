//! Read-backed write contracts: service errors carry no recovery authority.
use plasm_core::{Expr, Value};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    present: bool,
    writes: usize,
    reads: usize,
    observed_rows: Option<serde_json::Value>,
}
async fn engine(
    state: Arc<Mutex<State>>,
    commit: bool,
    message: &'static str,
) -> (ExecutionEngine, tokio::task::JoinHandle<()>) {
    let reads = state.clone();
    let app = axum::Router::new().route(
        "/language/v1/items",
        axum::routing::get(move || {
            let state = reads.clone();
            async move {
                let mut state = state.lock().unwrap();
                state.reads += 1;
                axum::Json(state.observed_rows.clone().unwrap_or_else(|| {
                    if state.present {
                        serde_json::json!([{"id":"item-1","title":"target"}])
                    } else {
                        serde_json::json!([])
                    }
                }))
            }
        })
        .post(move || {
            let state = state.clone();
            async move {
                let mut state = state.lock().unwrap();
                state.writes += 1;
                state.present |= commit;
                (
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    axum::Json(serde_json::json!({"message":message})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            hydrate: false,
            ..Default::default()
        })
        .unwrap(),
        server,
    )
}
fn catalog() -> plasm_core::CGS {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    plasm_core::load_schema_dir_unvalidated(&root.join("../../../fixtures/schemas/workflow_matrix"))
        .unwrap()
}
fn invocation() -> Expr {
    serde_json::from_value(serde_json::json!({"op":"create","capability":"workitem_create_idempotent","entity":"WorkItem","input":{"title":"target"}})).unwrap()
}

#[tokio::test]
async fn opaque_rejections_do_not_decide_write_outcome() {
    for message in [
        "already exists",
        "permission denied",
        "try again",
        "arbitrary opaque text",
    ] {
        for commit in [false, true] {
            let cgs = catalog();
            let state = Arc::new(Mutex::new(State::default()));
            let (engine, server) = engine(state.clone(), commit, message).await;
            let mut materialization = SessionMaterialization::new();
            let result = engine
                .execute(
                    &invocation(),
                    &cgs,
                    &mut materialization,
                    Some(ExecutionMode::Live),
                    StreamConsumeOpts::default(),
                    ExecuteOptions::for_catalog(&cgs).unwrap(),
                )
                .await;
            assert_eq!(
                result.is_ok(),
                commit,
                "only the read postcondition decides; opaque body: {message}"
            );
            server.abort();
            let state = state.lock().unwrap();
            assert_eq!(state.writes, 1, "reconciliation never retries a write");
            assert_eq!(
                state.reads, 1,
                "postcondition is observed after the failed dispatch"
            );
        }
    }
}

#[tokio::test]
async fn declared_state_preflight_skips_already_satisfied_targets() {
    let mut cgs = catalog();
    cgs.capabilities
        .get_mut("workitem_create_idempotent")
        .unwrap()
        .preflight = Some(plasm_core::preflight::PreflightPlan(vec![
        plasm_core::preflight::PreflightStep::ExistenceCheck {
            query: "workitem_query".into(),
            identity_from: Default::default(),
            on_exists: plasm_core::preflight::ExistenceOnExists::SkipWrite,
        },
    ]));
    let state = Arc::new(Mutex::new(State {
        present: true,
        ..Default::default()
    }));
    let (engine, server) = engine(state.clone(), false, "must never dispatch").await;
    let mut materialization = SessionMaterialization::new();
    for _ in 0..3 {
        let result = engine
            .execute(
                &invocation(),
                &cgs,
                &mut materialization,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.entities()[0].fields["outcome"].to_value(),
            Value::String("skipped".into())
        );
    }
    server.abort();
    assert_eq!(
        state.lock().unwrap().writes,
        0,
        "duplicates and preexisting targets do not dispatch"
    );
    assert_eq!(
        state.lock().unwrap().reads,
        3,
        "each guard observes authoritative state"
    );
}

#[tokio::test]
async fn missing_wrong_or_ambiguous_identity_never_authorizes_reuse() {
    for rows in [
        serde_json::json!([{"id":"item-1"}]),
        serde_json::json!([{"id":"item-1","title":"different"}]),
        serde_json::json!([{"id":"item-1","title":"target"},{"id":"item-2","title":"target"}]),
    ] {
        let cgs = catalog();
        let state = Arc::new(Mutex::new(State {
            observed_rows: Some(rows),
            ..Default::default()
        }));
        let (engine, server) = engine(state.clone(), true, "already exists").await;
        let result = engine
            .execute(
                &invocation(),
                &cgs,
                &mut SessionMaterialization::new(),
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await;
        server.abort();
        assert!(
            result.is_err(),
            "an unproven postcondition must preserve uncertainty"
        );
        assert_eq!(state.lock().unwrap().writes, 1);
    }
}

#[test]
fn catalog_rejects_service_message_authority() {
    let mut cgs = catalog();
    cgs.capabilities
        .get_mut("workitem_create_idempotent")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["conflict_rules"] =
        serde_json::json!([{ "when": {"contains":"already"}, "kind":"resource_exists" }]);
    assert!(plasm_compile::compile_cgs_capability_templates(&cgs).is_err());
}
