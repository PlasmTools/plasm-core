//! MCP tool-path integration tests (federated seeds, async wait/cancel).

use std::path::PathBuf;
use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{
    apply_capability_seeds, try_dispatch_operation_program, CapabilitySeed, RankedCapabilitiesArg,
};
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_dag::compile_plasm_surface_line_to_plan;
use crate::plasm_plan_run::evaluate_plasm_comp_dry;
use crate::run_delivery::{deliver_mcp_expensive_live_run, McpExpensiveLiveRunContext};
use crate::run_explorer_meta::build_run_explorer_accept_payload;
use crate::server_state::PlasmHostState;
use crate::trace_sink_emit::PlasmTraceContext;
use indexmap::IndexMap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::{CgsContext, CGS};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use uuid::Uuid;

fn matrix_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

fn matrix_federated_host() -> PlasmHostState {
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
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
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
        None,
        "hash".into(),
        None,
        None,
    )
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
        let plan = compile_plasm_surface_line_to_plan(pipeline, Some(cross), &es, sym, sym)
            .unwrap_or_else(|e| panic!("compile {sym}: {e}"));
        let qe = &plan["nodes"][0]["qualified_entity"];
        assert_eq!(qe["entry_id"], entry_id);
        assert_eq!(qe["entity"], "LangItem");

        let bundle = compile_plasm_expression(pipeline, Some(cross), &es, sym, sym)
            .unwrap_or_else(|e| panic!("compile bundle {sym}: {e}"));
        evaluate_plasm_comp_dry(&es, &bundle).expect("dry-run");
    }
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
            comp: serde_json::json!({}),
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
            comp: serde_json::json!({}),
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
async fn mcp_deliver_returns_none_for_bounded_plan() {
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
        "bounded deliver",
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
            .expect("bounded compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    assert!(
        !dry.review.execution_is_expensive(),
        "fixture program should be bounded: {:?}",
        dry.review
    );
    let accept_payload = build_run_explorer_accept_payload(&dry, Some(&es));
    let delivered = deliver_mcp_expensive_live_run(McpExpensiveLiveRunContext {
        es: Arc::clone(&es),
        st: Arc::clone(&st),
        prompt_hash: out.prompt_hash.clone(),
        session_id: out.session_id.clone(),
        session_ref: "l_bounded_deliver".into(),
        mcp_session_key: "mcp".into(),
        bundle,
        review: dry.review,
        accept_payload,
        dry_verdict: crate::plan_dry_display::PlanDryVerdict::Ok,
        plan_commit_ref: None,
        trace: PlasmTraceContext {
            trace_id: Uuid::nil(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: Some("l_bounded_deliver".into()),
        },
        wait_live: true,
        await_cfg: crate::mcp_run_await::AwaitConfig::default(),
    })
    .await
    .expect("deliver");
    assert!(delivered.is_none());
}

#[tokio::test]
async fn mcp_deliver_query_limit_not_expensive() {
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
        "query limit deliver",
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
    assert!(
        !dry.review.execution_is_expensive(),
        "query.limit should be bounded after prepare: {:?}",
        dry.review
    );
    let accept_payload = build_run_explorer_accept_payload(&dry, Some(&es));
    let delivered = deliver_mcp_expensive_live_run(McpExpensiveLiveRunContext {
        es: Arc::clone(&es),
        st: Arc::clone(&st),
        prompt_hash: out.prompt_hash.clone(),
        session_id: out.session_id.clone(),
        session_ref: "l_query_limit_deliver".into(),
        mcp_session_key: "mcp".into(),
        bundle,
        review: dry.review,
        accept_payload,
        dry_verdict: crate::plan_dry_display::PlanDryVerdict::Ok,
        plan_commit_ref: None,
        trace: PlasmTraceContext {
            trace_id: Uuid::nil(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: Some("l_query_limit_deliver".into()),
        },
        wait_live: true,
        await_cfg: crate::mcp_run_await::AwaitConfig::default(),
    })
    .await
    .expect("deliver");
    assert!(
        delivered.is_none(),
        "bounded query.limit must not server-await"
    );
}

#[test]
fn plasm_dry_run_continuation_error_blocks_wait_and_cancel_only() {
    assert!(crate::operation::plasm_dry_run_continuation_error("wait(l_x_o1)").is_some());
    assert!(crate::operation::plasm_dry_run_continuation_error("cancel(l_x_o1)").is_some());
    assert!(crate::operation::plasm_dry_run_continuation_error("e1").is_none());
    assert!(crate::operation::plasm_dry_run_continuation_error("page(l_x_pg1)").is_none());
}
