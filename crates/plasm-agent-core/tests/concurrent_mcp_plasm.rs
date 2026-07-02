//! CEP-13: parallel MCP `plasm_context` / `plasm` / `plasm_run` on one logical session.

use std::path::PathBuf;
use std::sync::Arc;

use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::{
    apply_capability_seeds, CapabilitySeed, RankedCapabilitiesArg,
};
use plasm_agent_core::operation::{
    compute_plan_commit_id_from_dry, PlanCommitRecord, PLAN_COMMIT_TTL,
};
use plasm_agent_core::plan_commit_store::{
    register_plan_commit_and_persist, verify_plan_commit_id,
};
use plasm_agent_core::plasm_compile::compile_plasm_expression;
use plasm_agent_core::plasm_plan_run::evaluate_plasm_comp_dry;
use plasm_agent_core::run_delivery::{
    deliver_live_run_await, LiveRunAwaitContext, LiveRunSpawnOpts,
};
use plasm_agent_core::run_explorer_meta::build_run_explorer_accept_payload;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_agent_core::trace_sink_emit::PlasmTraceContext;
use plasm_agent_core::PlanDryVerdict;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use uuid::Uuid;

fn matrix_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

fn matrix_federated_host() -> Arc<plasm_agent_core::server_state::PlasmHostState> {
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
    Arc::new(build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    }))
}

async fn open_matrix_session(
    st: &plasm_agent_core::server_state::PlasmHostState,
    logical_id: Uuid,
    seeds: Vec<CapabilitySeed>,
) -> plasm_agent_core::http_execute::ApplyCapabilitySeedsOutcome {
    apply_capability_seeds(
        st,
        None,
        None,
        seeds,
        None,
        None,
        Some(logical_id),
        "concurrent mcp eval",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply_capability_seeds")
}

#[tokio::test]
async fn parallel_context_open_single_flight() {
    let st = matrix_federated_host();
    let logical_id = Uuid::new_v4();
    let seeds = vec![CapabilitySeed {
        entry_id: "github".into(),
        entity: "LangItem".into(),
    }];
    let (a, b) = tokio::join!(
        open_matrix_session(st.as_ref(), logical_id, seeds.clone()),
        open_matrix_session(st.as_ref(), logical_id, seeds),
    );
    assert_eq!(a.prompt_hash, b.prompt_hash);
    assert_eq!(a.session_id, b.session_id);
}

#[tokio::test]
async fn concurrent_plasm_context_merge_seeds() {
    let st = matrix_federated_host();
    let logical_id = Uuid::new_v4();
    let (a, b) = tokio::join!(
        open_matrix_session(
            st.as_ref(),
            logical_id,
            vec![CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            }],
        ),
        open_matrix_session(
            st.as_ref(),
            logical_id,
            vec![CapabilitySeed {
                entry_id: "linear".into(),
                entity: "LangItem".into(),
            }],
        ),
    );
    assert_eq!(a.prompt_hash, b.prompt_hash);
    assert_eq!(a.session_id, b.session_id);
    let es = st
        .get_execute_session(&a.prompt_hash, &a.session_id)
        .await
        .expect("execute session");
    assert!(es.contexts_by_entry.contains_key("github"));
    assert!(es.contexts_by_entry.contains_key("linear"));
    let map = es
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    assert_eq!(map.entity_sym_for("github", "LangItem"), "e1");
    assert_eq!(map.entity_sym_for("linear", "LangItem"), "e2");
}

#[tokio::test]
async fn concurrent_mcp_plasm_dry_two_commits() {
    let st = matrix_federated_host();
    let out = open_matrix_session(
        st.as_ref(),
        Uuid::new_v4(),
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: "linear".into(),
                entity: "LangItem".into(),
            },
        ],
    )
    .await;
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");

    async fn register_dry(
        st: Arc<plasm_agent_core::server_state::PlasmHostState>,
        es: Arc<ExecuteSession>,
        out: plasm_agent_core::http_execute::ApplyCapabilitySeedsOutcome,
        sym: &'static str,
    ) -> (
        plasm_core::PlanCommitRef,
        plasm_agent_core::plasm_plan_run::DryPlasmPlanEvaluation,
    ) {
        let cross = st.sessions.symbol_map_cross_cache();
        let bundle =
            compile_plasm_expression(st.engine.prompt_pipeline(), Some(cross), &es, sym, sym)
                .expect("compile");
        let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
        let pc = es.mint_plan_commit_ref();
        register_plan_commit_and_persist(
            st.as_ref(),
            Arc::clone(&es),
            out.prompt_hash.as_str(),
            out.session_id.as_str(),
            PlanCommitRecord::from_dry_review(
                pc.clone(),
                compute_plan_commit_id_from_dry(&dry),
                es.domain_revision,
                &dry,
                sym.into(),
                PlanDryVerdict::Ok,
                std::time::Instant::now() + PLAN_COMMIT_TTL,
            )
            .expect("flow admission"),
        )
        .await
        .expect("persist plan commit");
        (pc, dry)
    }

    let ((pc1, dry1), (pc2, dry2)) = tokio::join!(
        register_dry(Arc::clone(&st), Arc::clone(&es), out.clone(), "e1"),
        register_dry(Arc::clone(&st), Arc::clone(&es), out, "e2"),
    );
    assert_ne!(pc1.as_str(), pc2.as_str());
    verify_plan_commit_id(&es, &pc1, compute_plan_commit_id_from_dry(&dry1)).expect("pc1 verifies");
    verify_plan_commit_id(&es, &pc2, compute_plan_commit_id_from_dry(&dry2)).expect("pc2 verifies");
}

#[tokio::test]
async fn concurrent_disjoint_plasm_run() {
    let st = matrix_federated_host();
    let out = open_matrix_session(
        st.as_ref(),
        Uuid::new_v4(),
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: "linear".into(),
                entity: "LangItem".into(),
            },
        ],
    )
    .await;
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("execute session");

    // The matrix fixture pins `http_backend: http://127.0.0.1:9` (the e2e Hermit harness intercepts
    // it; `plasm-agent-core` has no such interception), so a live run terminates in a transport
    // failure here. That is incidental — this test's CEP-13 contract is that two concurrent
    // `plasm_run` legs sharing one execute session mint and resolve *distinct, correctly-namespaced*
    // operation handles with no cross-contamination. The pre-fix bug surfaced as
    // `unknown operation handle 'l_..._o2'` (a resolution/namespacing corruption), not a transport
    // error, so we assert the terminal outcome is a clean transport failure and never a handle fault.
    async fn run_one(
        st: Arc<plasm_agent_core::server_state::PlasmHostState>,
        es: Arc<ExecuteSession>,
        out: plasm_agent_core::http_execute::ApplyCapabilitySeedsOutcome,
        program: &'static str,
    ) -> Result<
        plasm_agent_core::plasm_plan_run::PlasmPlanRunResult,
        plasm_agent_core::run_delivery::LiveRunError,
    > {
        let cross = st.sessions.symbol_map_cross_cache();
        let bundle = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(cross),
            &es,
            program,
            program,
        )
        .expect("compile");
        let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
        let accept_payload = build_run_explorer_accept_payload(&dry, Some(es.as_ref()));
        deliver_live_run_await(
            LiveRunAwaitContext::for_mcp_plasm_run(
                Arc::clone(&es),
                Arc::clone(&st),
                out.prompt_hash.clone(),
                out.session_id.clone(),
                "l_AAAAAAAAQACAAAAAAAAAAQ".into(),
                "mcp".into(),
                bundle,
                accept_payload,
                PlanDryVerdict::Ok,
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
        )
        .await
    }

    let st2 = Arc::clone(&st);
    let es2 = Arc::clone(&es);
    let out2 = out.clone();
    let (r1, r2) = tokio::join!(
        run_one(Arc::clone(&st), Arc::clone(&es), out.clone(), "e1"),
        run_one(st2, es2, out2, "e2"),
    );

    for r in [&r1, &r2] {
        let err = r
            .as_ref()
            .expect_err("dead-port backend terminates the live run in a transport failure");
        let msg = err.to_string();
        assert!(
            !msg.contains("unknown operation handle"),
            "CEP-13: concurrent legs must resolve their namespaced handles, got: {msg}"
        );
        assert!(
            msg.contains("HTTP request failed"),
            "expected a clean transport failure against the dead matrix backend, got: {msg}"
        );
    }
}
