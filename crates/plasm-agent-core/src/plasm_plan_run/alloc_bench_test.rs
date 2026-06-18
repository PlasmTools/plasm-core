//! In-crate heap regression guards (requires `--features alloc-bench`).
//!
//! ```text
//! cargo test -p plasm-agent-core --features alloc-bench plasm_plan_run::alloc_bench_test -- --nocapture
//! ```
//!
//! Live HTTP path (Hermit / backend required):
//! ```text
//! cargo test -p plasm-agent-core --release --features alloc-bench matrix_limit_3_live_heap_budget -- --ignored --nocapture
//! ```

#![cfg(all(test, feature = "alloc-bench"))]

use std::path::PathBuf;
use std::sync::Arc;

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{apply_capability_seeds, CapabilitySeed, RankedCapabilitiesArg};
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use crate::server_state::{CatalogBootstrap, PlasmHostState};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

fn matrix_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

fn matrix_host() -> PlasmHostState {
    let cgs = Arc::new(load_schema_dir(&matrix_fixture_dir()).expect("plasm_language_matrix"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        "github".into(),
        "GitHub".into(),
        vec!["github".into()],
        cgs.clone(),
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

async fn matrix_limit_3_session(
    st: &PlasmHostState,
) -> (
    Arc<PlasmHostState>,
    Arc<crate::execute_session::ExecuteSession>,
    String,
    String,
    crate::PlasmCompBundle,
    crate::plasm_plan_run::DryPlasmPlanEvaluation,
) {
    let st = Arc::new(st.clone());
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
        "alloc budget",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("seeds");
    let es = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let program = "items = e1.limit(3)\nitems";
    let bundle = compile_plasm_expression(pipeline, Some(cross), &es, program, program)
        .expect("compile");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    (
        st,
        es,
        out.prompt_hash,
        out.session_id,
        bundle,
        dry,
    )
}

#[tokio::test]
async fn matrix_limit_3_plan_setup_heap_budget() {
    let _profiler = dhat::Profiler::new_heap();
    let host = matrix_host();
    let (st, es, prompt_hash, session_id, _bundle, mut dry) =
        matrix_limit_3_session(&host).await;
    let node_results = dry.take_node_results_for_live();
    assert!(!node_results.is_empty());
    assert!(dry.node_results.is_empty());
    let _shared = PlanLineExecuteShared::prepare(&es, st.as_ref(), &session_id).await;
    drop(node_results);

    let stats = dhat::HeapStats::get();
    eprintln!(
        "matrix e1.limit(3) plan setup heap: total={} max={} blocks={}",
        stats.total_bytes, stats.max_bytes, stats.total_blocks
    );
    assert!(
        stats.total_bytes < 64 * 1024 * 1024,
        "plan setup heap total_bytes={} exceeds 64MiB budget",
        stats.total_bytes
    );
    let _ = (prompt_hash, session_id);
}

/// Full live run; requires a reachable catalog HTTP backend (e.g. Hermit on the engine base URL).
#[tokio::test]
#[ignore = "requires live HTTP backend for matrix catalog"]
async fn matrix_limit_3_live_heap_budget() {
    let _profiler = dhat::Profiler::new_heap();
    let host = matrix_host();
    let (st, es, prompt_hash, session_id, bundle, dry) =
        matrix_limit_3_session(&host).await;
    let result = run_plasm_comp(
        &es,
        st.as_ref(),
        &prompt_hash,
        &session_id,
        &bundle,
        true,
        None,
        None,
        Some(dry),
    )
    .await
    .expect("live run");
    assert!(!result.return_steps.is_empty() || result.run_markdown.is_some());

    let stats = dhat::HeapStats::get();
    eprintln!(
        "matrix e1.limit(3) live heap: total={} max={} blocks={}",
        stats.total_bytes, stats.max_bytes, stats.total_blocks
    );
    assert!(
        stats.total_bytes < 96 * 1024 * 1024,
        "live plan heap total_bytes={} exceeds 96MiB budget",
        stats.total_bytes
    );
}
