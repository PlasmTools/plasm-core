//! Actual HTTP background execution through the common operation evidence hooks.
use super::{
    model::*, repository::WorkflowRepository, LearningMode, ReviewSession, WorkflowLearning,
};
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_http_background_operation_seals_actual_source_and_receipt() {
    background_operation_seals_receipts(
        "class Inspect(Program):\n    def build(self):\n        return e1.query().take(1)\n",
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_multiple_results_preserve_one_complete_authored_program() {
    background_operation_seals_receipts(
        "class Inspect(Program):\n    def build(self):\n        first = e1.query().take(1)\n        second = e1.query().take(2)\n        return first, second\n",
        true,
    )
    .await;
}

async fn background_operation_seals_receipts(source: &str, multiple: bool) {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return;
    };
    // Host bootstrap starts configured workers: this fixture must never start one.
    assert_eq!(WorkflowLearning::default().mode, LearningMode::Disabled);
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("learning_operation_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let repo = Arc::new(WorkflowRepository::connect(&scoped_url).await.unwrap());
    repo.migrate().await.unwrap();

    let app = axum::Router::new().route(
        "/language/v1/items",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!([
                {"id":"a", "title":"observed fixture item"},
                {"id":"b", "title":"second fixture item"}
            ]))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cgs = crate::test_support::exposure_replay_fixtures::matrix_language_matrix_cgs();
    let registry = crate::test_support::exposure_replay_fixtures::matrix_federated_registry(cgs);
    let engine = plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig {
        base_url: Some(base),
        ..Default::default()
    })
    .unwrap();
    let mut host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
        engine,
        mode: plasm_runtime::ExecutionMode::Live,
        registry,
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    host.oss.workflow_learning = Arc::new(WorkflowLearning::with_test_repository(
        LearningMode::Shadow,
        Arc::clone(&repo),
    ));
    host.oss.execute_session_registry =
        crate::mcp_transport_store::ExecuteSessionRegistry::with_test_json_store().0;
    let host = Arc::new(host);
    let intent = "Inspect the first fixture item";
    let logical = host
        .logical_sessions
        .mint_session("", intent)
        .await
        .unwrap();
    let opened = crate::http_execute::apply_capability_seeds(
        &host,
        None,
        None,
        vec![crate::http_execute::CapabilitySeed {
            entry_id: "langmatrix_a".into(),
            entity: "LangItem".into(),
        }],
        None,
        None,
        Some(logical.logical_session_id.0),
        intent,
    )
    .await
    .unwrap();
    host.logical_execute_bindings
        .insert(
            logical.logical_session_id.0,
            opened.prompt_hash.clone(),
            opened.session_id.clone(),
        )
        .await;
    let session = host
        .get_execute_session(&opened.prompt_hash, &opened.session_id)
        .await
        .unwrap();
    let scope = super::scope(&session.tenant_scope, None);
    repo.bind_session(&scope, logical.logical_session_id)
        .await
        .unwrap();
    let bundle = crate::plasm_compile::compile_program(
        &Default::default(),
        None,
        &session,
        "operation_evidence",
        source,
    )
    .await
    .unwrap();
    let dry = crate::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();
    let compact = crate::plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        Some(&session),
        None,
    );
    let outcome =
        crate::run_delivery::deliver_http_live_run(crate::run_delivery::HttpLiveRunRequest {
            es: Arc::clone(&session),
            st: Arc::clone(&host),
            prompt_hash: opened.prompt_hash.clone(),
            session_id: opened.session_id.clone(),
            bundle,
            review: dry.review.clone(),
            dry,
            wait_live: false,
            verdict_for_gate: compact.verdict,
            plan_commit_ref: None,
        })
        .await
        .unwrap();
    let crate::run_delivery::HttpLiveRunOutcome::Accept { handle, .. } = outcome else {
        panic!("HTTP request must return background acceptance");
    };
    let output = crate::mcp_run_await::await_operation_terminal(
        crate::mcp_run_await::TerminalAwaitContext {
            es: Arc::clone(&session),
            st: Arc::clone(&host),
            handle,
            trace: crate::trace_sink_emit::PlasmTraceContext {
                trace_id: uuid::Uuid::nil(),
                call_index: None,
                mcp_session_id: None,
                logical_session_id: Some(logical.logical_session_id.to_string()),
                logical_session_ref: None,
            },
            cfg: crate::mcp_run_await::AwaitConfig {
                poll_interval: Duration::from_millis(20),
                max_wait: Duration::from_secs(30),
            },
        },
    )
    .await
    .unwrap();
    assert!(
        !output.code_plan_run_artifacts.is_empty(),
        "actual execution must retain evidence"
    );
    assert!(session.open_live_operation_handles().is_empty());
    let run_ids: Vec<_> = output
        .code_plan_run_artifacts
        .iter()
        .map(|artifact| crate::run_artifacts::RunArtifactId::from_wire(&artifact.run_id).unwrap())
        .collect();
    let sealed = host
        .workflow_learning
        .seal_review(
            &host,
            ReviewSession {
                scope,
                logical_session: logical.logical_session_id,
                prompt_hash: opened.prompt_hash,
                execute_session: opened.session_id,
                subtasks: vec![AchievedSubtask {
                    intent: intent.into(),
                    observed_outcome: "Fixture item a was returned".into(),
                    run_ids: run_ids.clone(),
                }],
            },
        )
        .await
        .unwrap();
    let claim = repo.claim(60).await.unwrap().unwrap();
    assert_eq!(claim.id, sealed.id);
    assert_eq!(claim.seal.assurance, OutcomeAssurance::AgentAttested);
    assert!(!claim.seal.executions.is_empty());
    if multiple {
        assert_eq!(claim.seal.executions.len(), 2);
        let checked: Vec<_> = claim
            .seal
            .executions
            .iter()
            .filter_map(|receipt| {
                super::teaching::CheckedTeachingExample::from_observation(&receipt.observation).ok()
            })
            .collect();
        assert_eq!(checked.len(), 2, "{:?}", claim.seal.executions);
        assert_eq!(checked[0], checked[1]);
        assert!(checked[0].program().source.contains("first ="));
        assert!(checked[0].program().source.contains("second ="));
        assert!(
            checked[0]
                .program()
                .source
                .contains("return (first, second)")
                || checked[0].program().source.contains("return first, second")
        );
    }
    for receipt in claim.seal.executions {
        assert_eq!(receipt.source, source);
        assert!(run_ids.contains(&receipt.run_id));
        assert!(receipt.compiled_plan.is_object());
        assert!(receipt
            .run_artifact
            .to_string()
            .contains("observed fixture item"));
    }
    mock.abort();
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
