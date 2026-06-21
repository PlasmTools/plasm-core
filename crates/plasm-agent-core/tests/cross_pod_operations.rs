//! Cross-pod async operation persistence (shared Redis session registry, no live executor).

use std::path::Path;
use std::sync::Arc;

use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::execute_session_create_response;
use plasm_agent_core::http_execute::CreateExecuteSessionBody;
use plasm_agent_core::http_execute::{handle_cancel_operation, handle_wait_operation};
use plasm_agent_core::mcp_transport_store::{
    descriptor_from_operation_state, ExecuteSessionRegistry, OperationPersistPatch,
    PersistedOperationDescriptor, PersistedOperationPhase, PersistedOperationProgress,
};
use plasm_agent_core::operation::OpAcceptContext;
use plasm_agent_core::operation_error::OperationError;
use plasm_agent_core::run_artifacts::{RunArtifactDocument, RunArtifactId, RunArtifactStore};
use plasm_agent_core::server_state::{CatalogBootstrap, PlasmHostState};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::loader::load_schema_dir;
use plasm_core::{Expr, QueryExpr};
use plasm_runtime::{
    CancelSignal, ExecutionConfig, ExecutionEngine, ExecutionMode, ExecutionSource, ExecutionStats,
};

fn overshow_registry() -> Arc<InMemoryCgsRegistry> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
    Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        "overshow".into(),
        "Overshow".into(),
        vec!["demo".into()],
        cgs,
    )]))
}

fn host_with_shared_registry(
    registry: Arc<InMemoryCgsRegistry>,
    execute_session_registry: ExecuteSessionRegistry,
    run_artifacts: Arc<RunArtifactStore>,
) -> PlasmHostState {
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    let mut st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts,
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    st.oss.execute_session_registry = execute_session_registry;
    st
}

fn run_artifact_doc(
    run_wire: &str,
    ph: &str,
    sid: &str,
    entities: Vec<serde_json::Value>,
) -> RunArtifactDocument {
    RunArtifactDocument {
        run_id: run_wire.to_string(),
        prompt_hash: ph.to_string(),
        session_id: sid.to_string(),
        entry_id: "overshow".into(),
        resource_index: None,
        principal: None,
        parsed_preimage: ParsedExpr {
            expr: Expr::Query(QueryExpr {
                entity: "Profile".into(),
                predicate: None,
                projection: None,
                pagination: None,
                hydrate: None,
                capability_name: None,
                catalog_entry_id: Some("overshow".into()),
            }),
            projection: None,
        },
        display_lines: vec![],
        request_fingerprints: vec![],
        entities,
        source: ExecutionSource::Live,
        stats: ExecutionStats::default(),
    }
}

async fn open_overshow_session(host: &PlasmHostState) -> (String, String, Arc<ExecuteSession>) {
    let created = execute_session_create_response(
        host,
        None,
        CreateExecuteSessionBody {
            entry_id: "overshow".into(),
            entities: vec!["Profile".into()],
            principal: None,
            logical_session_id: None,
            context_intent: None,
            ranked_capabilities: None,
            read_first_seeded_exposure: false,
        },
    )
    .await
    .expect("open session");
    let sess = host
        .get_execute_session(&created.prompt_hash, &created.session)
        .await
        .expect("session row");
    (created.prompt_hash, created.session, sess)
}

#[tokio::test]
async fn cross_pod_wait_terminal_hydrates_run_artifact() {
    let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
    let artifacts = Arc::new(RunArtifactStore::memory());
    let registry = overshow_registry();
    let host_a = host_with_shared_registry(
        registry.clone(),
        execute_registry.clone(),
        artifacts.clone(),
    );
    let host_b = host_with_shared_registry(registry, execute_registry.clone(), artifacts);

    let (ph, sid, sess) = open_overshow_session(&host_a).await;
    let handle = sess.mint_operation_handle_plain();
    let host_arc = Arc::new(host_a.clone());
    sess.try_begin_async_operation(
        handle.clone(),
        CancelSignal::new(),
        OpAcceptContext {
            host: Some(Arc::downgrade(&host_arc)),
            ..Default::default()
        },
    )
    .expect("accept");
    let run_wire = format!("pr{}", "ab".repeat(32));
    let run_id = RunArtifactId::from_wire(&run_wire).expect("wire id");
    host_a
        .run_artifacts
        .insert(
            &ph,
            &sid,
            run_id,
            &run_artifact_doc(
                &run_wire,
                &ph,
                &sid,
                vec![serde_json::json!({"id": "cross-pod", "display_name": "cross-pod"})],
            ),
        )
        .await
        .expect("artifact");
    sess.set_operation_run_artifact_id(&handle, run_wire.clone());
    sess.finalize_operation_succeeded(
        &handle,
        plasm_agent_core::plasm_plan_run::PlasmPlanRunResult {
            version: serde_json::json!({}),
            node_results: vec![serde_json::json!({"id": "cross-pod", "display_name": "cross-pod"})],
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: vec![],
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: Vec::new(),
        },
        None,
    );
    let op = sess.get_operation(&handle).expect("op");
    let desc = descriptor_from_operation_state(&handle, &op, 1_700_000_000);
    execute_registry
        .patch_session_operations(&ph, &sid, OperationPersistPatch::Upsert(desc))
        .await;
    host_a.sessions.purge_all().await;

    let sess_b = host_b
        .get_execute_session(&ph, &sid)
        .await
        .expect("rehydrate");
    let result = handle_wait_operation(&sess_b, Some(&host_b), None, &handle)
        .await
        .expect("wait terminal");
    assert_eq!(result.node_results.len(), 1);
    let md = result.run_markdown.as_deref().unwrap_or_default();
    assert!(md.contains("```tsv"), "expected TSV hydrate, got: {md}");
}

#[tokio::test]
async fn cross_pod_wait_running_returns_not_on_replica() {
    let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
    let registry = overshow_registry();
    let host_a = host_with_shared_registry(
        registry.clone(),
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );
    let host_b = host_with_shared_registry(
        registry,
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );

    let (ph, sid, sess) = open_overshow_session(&host_a).await;
    let handle = sess.mint_operation_handle_plain();
    sess.try_begin_async_operation(
        handle.clone(),
        CancelSignal::new(),
        OpAcceptContext::default(),
    )
    .expect("accept");
    sess.update_operation_progress(
        &handle,
        plasm_agent_core::operation::OperationProgress {
            step: 1,
            step_total: 3,
            label: Some("step".into()),
            rows_materialized: 0,
        },
    );
    let op = sess.get_operation(&handle).expect("op");
    execute_registry
        .patch_session_operations(
            &ph,
            &sid,
            OperationPersistPatch::Upsert(descriptor_from_operation_state(
                &handle,
                &op,
                1_700_000_000,
            )),
        )
        .await;
    host_a.sessions.purge_all().await;

    let sess_b = host_b
        .get_execute_session(&ph, &sid)
        .await
        .expect("rehydrate");
    let result = handle_wait_operation(&sess_b, Some(&host_b), None, &handle)
        .await
        .expect("wait running");
    let code = result
        .run_plasm_meta
        .as_ref()
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("code"))
        .and_then(|c| c.as_str());
    assert_eq!(code, Some(OperationError::CODE_NOT_ON_REPLICA));
}

#[tokio::test]
async fn cross_pod_cancel_running_returns_not_on_replica() {
    let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
    let registry = overshow_registry();
    let host_a = host_with_shared_registry(
        registry.clone(),
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );
    let host_b = host_with_shared_registry(
        registry,
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );

    let (ph, sid, sess) = open_overshow_session(&host_a).await;
    let handle = sess.mint_operation_handle_plain();
    sess.try_begin_async_operation(
        handle.clone(),
        CancelSignal::new(),
        OpAcceptContext::default(),
    )
    .expect("accept");
    let op = sess.get_operation(&handle).expect("op");
    execute_registry
        .patch_session_operations(
            &ph,
            &sid,
            OperationPersistPatch::Upsert(descriptor_from_operation_state(
                &handle,
                &op,
                1_700_000_000,
            )),
        )
        .await;
    host_a.sessions.purge_all().await;

    let sess_b = host_b
        .get_execute_session(&ph, &sid)
        .await
        .expect("rehydrate");
    let err = handle_cancel_operation(&sess_b, None, &handle)
        .await
        .expect_err("cancel foreign");
    assert!(matches!(err, OperationError::NotOnReplica { .. }));
}

#[tokio::test]
async fn cross_pod_rehydrate_preserves_operation_handle_monotonicity() {
    let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
    let registry = overshow_registry();
    let host_a = host_with_shared_registry(
        registry.clone(),
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );
    let host_b = host_with_shared_registry(
        registry,
        execute_registry.clone(),
        Arc::new(RunArtifactStore::memory()),
    );

    let (ph, sid, sess) = open_overshow_session(&host_a).await;
    let h1 = sess.mint_operation_handle_plain();
    assert!(h1.as_str() == "o1");
    execute_registry
        .patch_session_operations(
            &ph,
            &sid,
            OperationPersistPatch::Upsert(PersistedOperationDescriptor {
                handle: h1.as_str().to_string(),
                phase: PersistedOperationPhase::Succeeded,
                progress: PersistedOperationProgress::default(),
                started_at_unix: 0,
                error: None,
                run_artifact_id: None,
                plan_commit_ref: None,
                dry_verdict: None,
                display_map: Default::default(),
                agent_seq: 0,
                agent_last_line: String::new(),
            }),
        )
        .await;
    host_a.sessions.purge_all().await;

    let sess_b = host_b
        .get_execute_session(&ph, &sid)
        .await
        .expect("rehydrate");
    let h2 = sess_b.mint_operation_handle_plain();
    assert_ne!(h1, h2);
    assert_eq!(h2.as_str(), "o2");
}
