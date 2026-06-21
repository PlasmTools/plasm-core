//! MCP tool-path integration tests (federated seeds, async wait/cancel).

use std::path::PathBuf;
use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{
    apply_capability_seeds, try_dispatch_operation_program, ApplyCapabilitySeedsOutcome,
    CapabilitySeed, RankedCapabilitiesArg,
};
use crate::plan_dry_display::PlanDryCompactView;
use crate::plasm_compile::{compile_plasm_expression, compile_plasm_program};
use crate::plasm_plan::ValidatedPlanNode;
use crate::plasm_plan_run::{evaluate_plasm_comp_dry, DryPlasmPlanEvaluation};
use crate::run_delivery::{
    deliver_live_run_await, LiveRunAwaitContext, LiveRunSpawnOpts, RunDeliveryPolicy,
};
use crate::run_explorer_meta::build_run_explorer_accept_payload;
use crate::server_state::PlasmHostState;
use crate::trace_hub::{McpPlasmTraceSink, PlanRunTraceHooks, TraceSessionMeta};
use crate::trace_sink_emit::PlasmTraceContext;
use crate::PlasmCompBundle;
use indexmap::IndexMap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::PlanCommitRef;
use plasm_core::{CgsContext, CGS};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use uuid::Uuid;

fn matrix_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

fn matrix_federated_host() -> PlasmHostState {
    matrix_federated_host_with_base(None)
}

fn matrix_federated_host_with_base(base_url: Option<&str>) -> PlasmHostState {
    let cgs = Arc::new(load_schema_dir(&matrix_fixture_dir()).expect("plasm_language_matrix"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![
        (
            "github".into(),
            "GitHub".into(),
            vec!["github".into()],
            cgs.clone(),
        ),
        (
            "linear".into(),
            "Linear".into(),
            vec!["linear".into()],
            cgs.clone(),
        ),
    ]);
    let config = ExecutionConfig {
        base_url: base_url.map(str::to_string),
        ..Default::default()
    };
    let engine = ExecutionEngine::new(config).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

async fn spawn_matrix_langitem_mock() -> String {
    use axum::{extract::Path, routing::get, Json, Router};

    async fn list_items() -> Json<serde_json::Value> {
        Json(serde_json::json!([{"id": "a", "title": "trace-test"}]))
    }

    async fn get_item(Path(id): Path<String>) -> Json<serde_json::Value> {
        Json(serde_json::json!({"id": id, "title": "trace-test"}))
    }

    let app = Router::new()
        .route("/language/v1/items", get(list_items))
        .route("/language/v1/items/{id}", get(get_item));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind matrix mock");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("matrix mock serve");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    base
}

fn minimal_execute_session() -> ExecuteSession {
    let cgs = Arc::new(CGS::new());
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "default".into(),
        Arc::new(CgsContext::entry("default", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs,
        ctxs,
        "default".into(),
        String::new(),
        String::new(),
        None,
        vec!["Pet".into()],
        None,
        None,
        "hash".into(),
        None,
        None,
    )
}

struct MatrixPcNFixture {
    st: Arc<PlasmHostState>,
    out: ApplyCapabilitySeedsOutcome,
    es: Arc<ExecuteSession>,
    program: String,
    pc: PlanCommitRef,
    dry: DryPlasmPlanEvaluation,
    compact: PlanDryCompactView,
    bundle: PlasmCompBundle,
}

impl MatrixPcNFixture {
    async fn open_with_test_store(intent: &str, compile_tag: &str) -> Self {
        use crate::mcp_transport_store::ExecuteSessionRegistry;

        let mut st_inner = matrix_federated_host();
        st_inner.oss.execute_session_registry = ExecuteSessionRegistry::with_test_json_store().0;
        Self::open(Arc::new(st_inner), intent, compile_tag).await
    }

    async fn open(st: Arc<PlasmHostState>, intent: &str, compile_tag: &str) -> Self {
        let seeds = vec![CapabilitySeed {
            entry_id: "github".into(),
            entity: "LangItem".into(),
        }];
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds,
            None,
            None,
            None,
            intent,
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("apply_capability_seeds");
        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("execute session");
        let program = "e1(p1=\"a\")".to_string();
        let pipeline = st.engine.prompt_pipeline();
        let cross = st.sessions.symbol_map_cross_cache();
        let bundle =
            compile_plasm_expression(pipeline, Some(cross), &es, compile_tag, program.as_str())
                .expect("compile");
        let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
        let compact = crate::plan_dry_display::build_plan_dry_compact_view(
            dry.validated_plan(),
            &dry.topological_order,
            &dry.review,
            &dry.graph_summary,
            Some(&es),
        );
        let pc = es.mint_plan_commit_ref();
        Self {
            st,
            out,
            es,
            program,
            pc,
            dry,
            compact,
            bundle,
        }
    }

    async fn persist_plan_commit(&self) {
        use crate::operation::{
            compute_plan_commit_id_from_dry, PlanCommitDryCache, PlanCommitRecord, PLAN_COMMIT_TTL,
        };
        use crate::plan_commit_store::register_plan_commit_and_persist;

        register_plan_commit_and_persist(
            self.st.as_ref(),
            Arc::clone(&self.es),
            self.out.prompt_hash.as_str(),
            self.out.session_id.as_str(),
            PlanCommitRecord {
                commit_ref: self.pc.clone(),
                commit_id: compute_plan_commit_id_from_dry(&self.dry),
                domain_revision: self.es.domain_revision,
                artifact: self.dry.artifact().clone(),
                program: self.program.clone(),
                dry_review: self.dry.review.clone(),
                verdict: self.compact.verdict,
                expires_at: std::time::Instant::now() + PLAN_COMMIT_TTL,
                dry_cache: PlanCommitDryCache::from_dry(&self.dry),
            },
        )
        .await
        .expect("persist pcN");
    }

    async fn rehydrated_session(&self) -> Arc<ExecuteSession> {
        self.st.sessions.purge_all().await;
        self.st
            .get_execute_session(&self.out.prompt_hash, &self.out.session_id)
            .await
            .expect("rehydrated execute session")
    }
}

#[tokio::test]
async fn mcp_apply_capability_seeds_federates_multi_catalog_and_dry_runs_distinct_e_symbols() {
    let st = Arc::new(matrix_federated_host());
    let seeds = vec![
        CapabilitySeed {
            entry_id: "github".into(),
            entity: "LangItem".into(),
        },
        CapabilitySeed {
            entry_id: "linear".into(),
            entity: "LangItem".into(),
        },
    ];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "federated matrix eval",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");

    assert_eq!(out.primary_entry_id, "github");
    assert!(out.waves.len() >= 2, "expected open + federate waves");
    let distinct_catalogs: std::collections::BTreeSet<_> =
        out.waves.iter().map(|w| w.entry_id.as_str()).collect();
    assert!(distinct_catalogs.contains("github"));
    assert!(distinct_catalogs.contains("linear"));

    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    assert!(es.contexts_by_entry.contains_key("github"));
    assert!(es.contexts_by_entry.contains_key("linear"));

    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    for (sym, entry_id) in [("e1", "github"), ("e2", "linear")] {
        let bundle = compile_plasm_expression(pipeline, Some(cross), &es, sym, sym)
            .unwrap_or_else(|e| panic!("compile bundle {sym}: {e}"));
        let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry-run");
        let qe = dry
            .validated_plan()
            .nodes
            .iter()
            .find_map(|n| match n {
                ValidatedPlanNode::Surface(s) => s.qualified_entity.as_ref(),
                _ => None,
            })
            .unwrap_or_else(|| panic!("surface node with qualified_entity for {sym}"));
        assert_eq!(qe.entry_id, entry_id);
        assert_eq!(qe.entity, "LangItem");
    }

    let exp = es.teaching_exposure.as_ref().expect("exposure");
    let map = exp.symbol_map_arc();
    let r_sym = map.ident_sym_relation_for("linear", "LangItem", "children");
    let rel_program = format!("parent = e2(\"i1\")\nkids = parent.{r_sym}\nkids[id,title]");
    let rel_bundle =
        compile_plasm_program(pipeline, Some(cross), &es, "federated_rel", &rel_program)
            .unwrap_or_else(|e| panic!("compile federated relation hop: {e}"));
    evaluate_plasm_comp_dry(&es, &rel_bundle).expect("dry-run federated relation hop");
}

#[tokio::test]
async fn mcp_federated_post_async_finalize_compiles_e2_with_cross_cache() {
    let st = Arc::new(matrix_federated_host());
    let seeds = vec![
        CapabilitySeed {
            entry_id: "github".into(),
            entity: "LangItem".into(),
        },
        CapabilitySeed {
            entry_id: "linear".into(),
            entity: "LangItem".into(),
        },
    ];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "federated matrix eval",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");

    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    let session_ref = "l_federated_async_e2";
    let handle = es.mint_operation_handle(session_ref);
    es.try_begin_async_operation(
        handle.clone(),
        plasm_runtime::CancelSignal::new(),
        crate::operation::OpAcceptContext::default(),
    )
    .expect("register async op");
    es.finalize_operation_succeeded(
        &handle,
        crate::plasm_plan_run::PlasmPlanRunResult {
            version: serde_json::json!({}),
            node_results: vec![serde_json::json!({"id": "done"})],
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some("## done".into()),
            run_plasm_meta: None,
            return_steps: Vec::new(),
        },
        None,
    );

    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle = compile_plasm_expression(pipeline, Some(cross), &es, "e2", "e2")
        .expect("compile e2 after async finalize");
    evaluate_plasm_comp_dry(&es, &bundle).expect("dry-run e2");
}

#[tokio::test]
async fn mcp_async_wait_poll_reaches_terminal_result() {
    let es = minimal_execute_session();
    let session_ref = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let handle = es.mint_operation_handle(session_ref);
    es.try_begin_async_operation(
        handle.clone(),
        plasm_runtime::CancelSignal::new(),
        crate::operation::OpAcceptContext::default(),
    )
    .expect("register async op");

    let trace = PlasmTraceContext {
        trace_id: Uuid::nil(),
        call_index: None,
        mcp_session_id: None,
        logical_session_id: None,
        logical_session_ref: Some(session_ref.into()),
    };
    let wait_program = format!("wait({})", handle.as_str());

    let running = try_dispatch_operation_program(&es, None, Some(&trace), &wait_program, None)
        .await
        .expect("wait dispatch")
        .expect("wait result");
    let running_md = running
        .run_markdown
        .as_deref()
        .unwrap_or_default()
        .to_string();
    assert!(
        running_md.contains("Still open") || running_md.contains('~') || running_md.contains('='),
        "expected in-flight poll markdown, got: {running_md}"
    );

    es.finalize_operation_succeeded(
        &handle,
        crate::plasm_plan_run::PlasmPlanRunResult {
            version: serde_json::json!({}),
            node_results: vec![serde_json::json!({"id": "done"})],
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some("## done (1 rows)".into()),
            run_plasm_meta: None,
            return_steps: Vec::new(),
        },
        None,
    );

    let terminal = try_dispatch_operation_program(&es, None, Some(&trace), &wait_program, None)
        .await
        .expect("terminal wait dispatch")
        .expect("terminal wait result");
    assert_eq!(terminal.run_markdown.as_deref(), Some("## done (1 rows)"));
}

#[tokio::test]
async fn mcp_policy_always_spawns_async_when_wait_live() {
    use crate::run_delivery::should_spawn_async_for_policy;

    let st = Arc::new(matrix_federated_host());
    let seeds = vec![CapabilitySeed {
        entry_id: "github".into(),
        entity: "LangItem".into(),
    }];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "await policy",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle =
        compile_plasm_expression(pipeline, Some(cross), &es, "e1(p1=\"a\")", "e1(p1=\"a\")")
            .expect("compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    assert!(
        !dry.review.execution_is_expensive(),
        "fixture get should be cheap: {:?}",
        dry.review
    );
    assert!(should_spawn_async_for_policy(
        RunDeliveryPolicy::McpAwaitTerminal,
        true,
        &dry.review
    ));
}

#[tokio::test]
async fn mcp_query_limit_uses_async_await_path() {
    let st = Arc::new(matrix_federated_host());
    let seeds = vec![CapabilitySeed {
        entry_id: "github".into(),
        entity: "LangItem".into(),
    }];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "query limit await",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let program = "items = e1.limit(3)\nitems";
    let bundle = compile_plasm_expression(pipeline, Some(cross), &es, program, program)
        .expect("query+limit compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    assert!(!dry.review.execution_is_expensive());
    let accept_payload = build_run_explorer_accept_payload(&dry, Some(&es));
    let compact = crate::plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        Some(&es),
    );
    let handle_before = es.open_live_operation_handles().len();
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&es),
                Arc::clone(&st),
                out.prompt_hash.clone(),
                out.session_id.clone(),
                "l_AAAAAAAAQACAAAAAAAAAAQ".to_string(),
                "mcp".to_string(),
                bundle,
                accept_payload,
                compact.verdict,
                None,
                PlasmTraceContext {
                    trace_id: Uuid::nil(),
                    call_index: None,
                    mcp_session_id: None,
                    logical_session_id: None,
                    logical_session_ref: Some("l_AAAAAAAAQACAAAAAAAAAAQ".into()),
                },
                dry,
            ),
            LiveRunSpawnOpts::default(),
        ),
    )
    .await;
    let Ok(delivered_inner) = delivered else {
        panic!("query.limit await hung past 30s");
    };
    if let Ok(result) = delivered_inner {
        assert!(
            !result.return_steps.is_empty() || result.run_markdown.is_some(),
            "expected terminal rows or markdown on success"
        );
    }
    assert!(
        handle_before == 0 || es.open_live_operation_handles().is_empty(),
        "terminal await must not leave running ops"
    );
}

#[tokio::test]
async fn matrix_query_limit_on_injected_live_plan_pool() {
    let mut st = matrix_federated_host();
    st.oss.live_plan_pool = Arc::new(crate::live_plan_run_worker::LivePlanRunPool::new());
    let st = Arc::new(st);
    let seeds = vec![CapabilitySeed {
        entry_id: "github".into(),
        entity: "LangItem".into(),
    }];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "injected pool",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let program = "items = e1.limit(3)\nitems";
    let bundle = compile_plasm_expression(pipeline, Some(cross), &es, program, program)
        .expect("query+limit compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    let accept_payload = build_run_explorer_accept_payload(&dry, Some(&es));
    let compact = crate::plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        Some(&es),
    );
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&es),
                Arc::clone(&st),
                out.prompt_hash.clone(),
                out.session_id.clone(),
                "l_AAAAAAAAQACAAAAAAAAAAQ".to_string(),
                "mcp".to_string(),
                bundle,
                accept_payload,
                compact.verdict,
                None,
                PlasmTraceContext {
                    trace_id: Uuid::nil(),
                    call_index: None,
                    mcp_session_id: None,
                    logical_session_id: None,
                    logical_session_ref: Some("l_AAAAAAAAQACAAAAAAAAAAQ".into()),
                },
                dry,
            ),
            LiveRunSpawnOpts::default(),
        ),
    )
    .await;
    assert!(
        delivered.is_ok(),
        "cheap matrix live must finish on host live_plan_pool"
    );
}

#[cfg(not(debug_assertions))]
#[tokio::test]
async fn matrix_query_limit_on_release_stack_budget() {
    use crate::live_plan_run_worker::{LivePlanRunPool, DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_RELEASE};

    let mut st = matrix_federated_host();
    st.oss.live_plan_pool = Arc::new(LivePlanRunPool::with_stack_bytes(
        DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_RELEASE,
    ));
    let st = Arc::new(st);
    let seeds = vec![CapabilitySeed {
        entry_id: "github".into(),
        entity: "LangItem".into(),
    }];
    let out = apply_capability_seeds(
        st.as_ref(),
        None,
        None,
        seeds,
        None,
        None,
        None,
        "release stack budget",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds");
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let program = "items = e1.limit(3)\nitems";
    let bundle = compile_plasm_expression(pipeline, Some(cross), &es, program, program)
        .expect("query+limit compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    let accept_payload = build_run_explorer_accept_payload(&dry, Some(&es));
    let compact = crate::plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        Some(&es),
    );
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&es),
                Arc::clone(&st),
                out.prompt_hash.clone(),
                out.session_id.clone(),
                "l_AAAAAAAAQACAAAAAAAAAAQ".to_string(),
                "mcp".to_string(),
                bundle,
                accept_payload,
                compact.verdict,
                None,
                PlasmTraceContext {
                    trace_id: Uuid::nil(),
                    call_index: None,
                    mcp_session_id: None,
                    logical_session_id: None,
                    logical_session_ref: Some("l_AAAAAAAAQACAAAAAAAAAAQ".into()),
                },
                dry,
            ),
            LiveRunSpawnOpts::default(),
        ),
    )
    .await;
    assert!(
        delivered.is_ok(),
        "cheap matrix live must finish on 4 MiB worker stack"
    );
}

#[tokio::test]
async fn mcp_dry_run_plan_commit_merges_from_durable_when_in_memory_stale() {
    use crate::plan_commit_store::resolve_committed_plan;

    let fx = MatrixPcNFixture::open_with_test_store("pcN stale in-memory", "mcp_pcN_stale").await;
    fx.persist_plan_commit().await;
    resolve_committed_plan(&fx.es, &fx.pc).expect("in-memory pcN after mint");

    fx.es.remove_plan_commit(&fx.pc);
    assert!(
        resolve_committed_plan(&fx.es, &fx.pc).is_err(),
        "stale pod must miss pcN until durable merge"
    );

    let merged = fx
        .st
        .get_execute_session(&fx.out.prompt_hash, &fx.out.session_id)
        .await
        .expect("execute session");
    assert!(
        Arc::ptr_eq(&merged, &fx.es),
        "same in-memory row, merged fields"
    );
    resolve_committed_plan(&merged, &fx.pc).expect("pcN merged from durable descriptor");
}

#[tokio::test]
async fn mcp_dry_run_plan_commit_survives_rehydrate_for_plasm_run() {
    use crate::plan_commit_store::resolve_committed_plan;

    let fx = MatrixPcNFixture::open_with_test_store("pcN rehydrate", "mcp_pcN").await;
    fx.persist_plan_commit().await;

    resolve_committed_plan(&fx.es, &fx.pc).expect("in-memory pcN");

    let es2 = fx.rehydrated_session().await;
    let committed = resolve_committed_plan(&es2, &fx.pc).expect("rehydrated pcN for plasm_run");
    assert_eq!(committed.program, fx.program);
}

#[tokio::test]
async fn mcp_pc_n_committed_await_uses_stored_review() {
    use crate::plan_commit_store::resolve_committed_plan;

    let fx = MatrixPcNFixture::open_with_test_store("pcN await gate", "mcp_pcN_gate").await;
    assert!(!fx.dry.review.execution_is_expensive());
    fx.persist_plan_commit().await;
    resolve_committed_plan(&fx.es, &fx.pc).expect("pcN");

    let es2 = fx.rehydrated_session().await;
    let committed2 = resolve_committed_plan(&es2, &fx.pc).expect("rehydrated pcN");
    assert!(!committed2.dry_review.execution_is_expensive());

    let accept_payload = build_run_explorer_accept_payload(&fx.dry, Some(&es2));
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&es2),
                Arc::clone(&fx.st),
                fx.out.prompt_hash.clone(),
                fx.out.session_id.clone(),
                "l_AAAAAAAAQACAAAAAAAAAAQ".to_string(),
                "mcp".to_string(),
                fx.bundle,
                accept_payload,
                committed2.verdict,
                Some(fx.pc),
                PlasmTraceContext {
                    trace_id: Uuid::nil(),
                    call_index: None,
                    mcp_session_id: None,
                    logical_session_id: None,
                    logical_session_ref: Some("l_AAAAAAAAQACAAAAAAAAAAQ".into()),
                },
                fx.dry,
            ),
            LiveRunSpawnOpts::default(),
        ),
    )
    .await;
    assert!(
        matches!(delivered, Ok(Ok(_)) | Ok(Err(_))),
        "committed pcN await hung past 30s: {delivered:?}"
    );
}

#[tokio::test]
async fn mcp_plan_trace_hooks_emit_plasm_line_and_network_totals() {
    let base_url = spawn_matrix_langitem_mock().await;
    let st = Arc::new(matrix_federated_host_with_base(Some(base_url.as_str())));
    let fx =
        MatrixPcNFixture::open(Arc::clone(&st), "trace hooks plasm_line", "mcp_trace_hooks").await;
    fx.persist_plan_commit().await;

    let ls_key = "550e8400-e29b-41d4-a716-446655440099";
    let session_ref = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let tenant = "anonymous";
    let trace_meta = TraceSessionMeta {
        tenant_id: tenant.into(),
        project_slug: "main".into(),
        mcp_config: None,
    };
    let trace_id = fx
        .st
        .trace_hub
        .ensure_logical_session(ls_key, None, trace_meta)
        .await;
    let call_index = fx
        .st
        .trace_hub
        .trace_record_plasm_invocation(ls_key, false, 1, None, fx.program.len() as u64, None)
        .await;
    let plan_trace = PlanRunTraceHooks {
        trace: PlasmTraceContext {
            trace_id,
            call_index: Some(call_index as i64),
            mcp_session_id: None,
            logical_session_id: Some(ls_key.into()),
            logical_session_ref: Some(session_ref.into()),
        },
        sink: McpPlasmTraceSink {
            hub: Arc::clone(&fx.st.trace_hub),
            mcp_key: ls_key.to_string(),
            call_index,
        },
        meta_index: None,
    };

    let accept_payload = build_run_explorer_accept_payload(&fx.dry, Some(fx.es.as_ref()));
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&fx.es),
                Arc::clone(&fx.st),
                fx.out.prompt_hash.clone(),
                fx.out.session_id.clone(),
                session_ref.to_string(),
                "mcp-trace-hooks".to_string(),
                fx.bundle,
                accept_payload,
                fx.compact.verdict,
                Some(fx.pc),
                PlasmTraceContext {
                    trace_id,
                    call_index: Some(call_index as i64),
                    mcp_session_id: None,
                    logical_session_id: Some(ls_key.into()),
                    logical_session_ref: Some(session_ref.into()),
                },
                fx.dry,
            ),
            LiveRunSpawnOpts {
                plan_trace: Some(plan_trace),
            },
        ),
    )
    .await
    .expect("plan trace live run hung past 30s")
    .expect("plan trace live run failed");

    assert!(
        !delivered.return_steps.is_empty() || delivered.run_markdown.is_some(),
        "expected terminal rows or markdown"
    );

    let detail = fx
        .st
        .trace_hub
        .get_detail(trace_id, Some(tenant))
        .await
        .expect("trace detail after live run");
    assert!(
        detail
            .records
            .iter()
            .any(|r| { r.get("kind").and_then(|v| v.as_str()) == Some("plasm_line") }),
        "expected plasm_line in trace records: {:?}",
        detail.records
    );
    let plasm_line = detail
        .records
        .iter()
        .find(|r| r.get("kind").and_then(|v| v.as_str()) == Some("plasm_line"))
        .expect("plasm_line record");
    let http_calls = plasm_line
        .get("http_calls")
        .and_then(|v| v.as_array())
        .expect("plasm_line.http_calls array");
    assert!(
        !http_calls.is_empty(),
        "expected non-empty http_calls on plasm_line: {plasm_line:?}"
    );
    assert!(
        detail.summary.totals.network_requests > 0,
        "expected non-zero network_requests, got {:?}",
        detail.summary.totals
    );
    assert!(
        detail.summary.totals.http_trace_entry_count > 0,
        "expected non-zero http_trace_entry_count, got {:?}",
        detail.summary.totals
    );
    assert!(
        crate::terminal_plan_run::plan_run_result_is_terminal(&delivered),
        "plasm_run must return terminal rows, not operation poll: {:?}",
        delivered.run_markdown
    );
}

#[test]
fn plan_run_result_is_terminal_rejects_operation_poll_markdown() {
    use crate::operation_progress::{op_plasm_meta_short, OpWireSig};
    use plasm_core::OperationHandle;

    let handle = OperationHandle::parse("o1").expect("handle");
    let mut root = serde_json::Map::new();
    root.insert(
        "plasm".into(),
        serde_json::Value::Object(op_plasm_meta_short(
            &handle,
            OpWireSig::Unchanged,
            2,
            None,
            None,
        )),
    );
    let poll = crate::plasm_plan_run::PlasmPlanRunResult {
        version: serde_json::json!({}),
        node_results: Vec::new(),
        graph_summary: serde_json::json!({}),
        comp: None,
        code_plan_run_artifacts: Vec::new(),
        run_markdown: Some(format!("`{}` =", handle.as_str())),
        run_plasm_meta: Some(root),
        return_steps: Vec::new(),
    };
    assert!(!crate::terminal_plan_run::plan_run_result_is_terminal(
        &poll
    ));
}

#[test]
fn plasm_dry_run_continuation_error_blocks_wait_and_cancel_only() {
    assert!(crate::operation::plasm_dry_run_continuation_error("wait(l_x_o1)").is_some());
    assert!(crate::operation::plasm_dry_run_continuation_error("cancel(l_x_o1)").is_some());
    assert!(crate::operation::plasm_dry_run_continuation_error("e1").is_none());
    assert!(crate::operation::plasm_dry_run_continuation_error("page(l_x_pg1)").is_none());
}
