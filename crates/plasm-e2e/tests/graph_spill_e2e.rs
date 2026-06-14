//! Graph spill + hot trim + plan rehydrate (Hermit pokeapi_mini + file persistence).
//!
//! Requires `PLASM_GRAPH_CACHE_URL` and a low `PLASM_GRAPH_HOT_MAX_ENTITIES`.
//! This integration-test binary contains a single test; no `serial_test` lock is required.
//! If a prior run was interrupted, kill stale `graph_spill_e2e` processes — they can hold
//! resources and appear as indefinite hangs under `cargo test`.

#![allow(dead_code)] // common::hermit re-exports petstore helpers unused by this binary.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_agent::{
    execute_session::ExecuteSession,
    graph_page_spill_for_execute,
    http::{build_plasm_host_state, PlasmHostBootstrap},
    plasm_compile::compile_plasm_program,
    plasm_plan_run::run_plasm_comp,
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
    session_graph_persistence::SessionGraphPersistence,
};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::{CgsContext, Expr, PromptPipelineConfig, QueryExpr, QueryPagination, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, StreamConsumeOpts,
};

use common::hermit;

const ENTRY_ID: &str = "default";
const PROMPT_HASH: &str = "graph_spill_e2e_ph";
const SESSION_ID: &str = "graph_spill_e2e_sess";
const HOT_MAX: usize = 1;

struct GraphSpillEnv {
    _dir: tempfile::TempDir,
    persistence: Arc<SessionGraphPersistence>,
}

impl GraphSpillEnv {
    fn install() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store_root = dir.path().join("graph-cache");
        std::fs::create_dir_all(&store_root).expect("graph-cache dir");
        let url = format!("file://{}", store_root.display());
        // SAFETY: single-test binary; env vars are not shared with parallel tests.
        unsafe {
            std::env::set_var("PLASM_GRAPH_CACHE_URL", url);
            std::env::set_var("PLASM_GRAPH_HOT_MAX_ENTITIES", HOT_MAX.to_string());
        }
        let persistence = plasm_agent::session_graph_persistence::init_from_env()
            .expect("init graph persistence")
            .expect("PLASM_GRAPH_CACHE_URL should resolve");
        Self {
            _dir: dir,
            persistence,
        }
    }
}

impl Drop for GraphSpillEnv {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("PLASM_GRAPH_CACHE_URL");
            std::env::remove_var("PLASM_GRAPH_HOT_MAX_ENTITIES");
        }
    }
}

fn load_pokeapi_mini_cgs() -> Arc<CGS> {
    let paths = [
        "fixtures/schemas/pokeapi_mini",
        "../../fixtures/schemas/pokeapi_mini",
    ];
    for path in &paths {
        let p = Path::new(path);
        if p.exists() {
            return Arc::new(plasm_core::loader::load_schema_dir(p).expect("pokeapi_mini CGS"));
        }
    }
    panic!("fixtures/schemas/pokeapi_mini not found");
}

fn pokeapi_execute_session(cgs: Arc<CGS>) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        ENTRY_ID.into(),
        Arc::new(CgsContext::entry(ENTRY_ID, cgs.clone())),
    );
    ExecuteSession::new(
        PROMPT_HASH.into(),
        String::new(),
        cgs.clone(),
        ctxs,
        ENTRY_ID.into(),
        String::new(),
        String::new(),
        None,
        vec!["Berry".into()],
        None,
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

fn pokeapi_host_state(
    engine: ExecutionEngine,
    cgs: Arc<CGS>,
    persistence: Arc<SessionGraphPersistence>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        ENTRY_ID.into(),
        "PokeAPI Mini".into(),
        vec!["test".into()],
        cgs,
    )]));
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: Some(persistence),
        oss_local_filesystem_defaults: false,
    })
}

fn make_engine(base_url: &str) -> ExecutionEngine {
    ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url.to_string()),
        ..Default::default()
    })
    .expect("ExecutionEngine")
}

#[test]
fn graph_spill_bounded_hot_and_plan_filter_rehydrate() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("graph spill runtime");
            rt.block_on(graph_spill_bounded_hot_and_plan_filter_rehydrate_async());
        })
        .expect("spawn graph spill thread")
        .join()
        .expect("graph spill thread join");
}

async fn graph_spill_bounded_hot_and_plan_filter_rehydrate_async() {
    let env = GraphSpillEnv::install();
    let base = hermit::pokeapi_hermit_graph_spill_base_url().await.clone();
    let cgs = load_pokeapi_mini_cgs();
    let engine = make_engine(&base);
    let es = pokeapi_execute_session(cgs.clone());
    let st = pokeapi_host_state(make_engine(&base), cgs.clone(), env.persistence.clone());

    let spill = graph_page_spill_for_execute(
        Some(&env.persistence),
        es.core.clone(),
        PROMPT_HASH,
        SESSION_ID,
    )
    .expect("spill handle when persistence is set");

    let mut mat = es.graph_cache.lock().await;
    let mut query = QueryExpr::all("Berry");
    query.pagination = Some(QueryPagination::default());

    let runtime_result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: true,
                ..Default::default()
            },
            ExecuteOptions {
                graph_page_spill: Some(spill),
                ..Default::default()
            },
        )
        .await
        .expect("graph-backed fetch-all");

    let total = runtime_result.count;
    assert!(total >= 1, "expected at least one berry from Hermit");
    assert!(
        runtime_result.entities.is_empty(),
        "graph-backed pages should not retain full entity vectors in the execution result"
    );
    assert!(
        mat.graph.stats().total_entities <= HOT_MAX,
        "hot cache should trim to PLASM_GRAPH_HOT_MAX_ENTITIES={HOT_MAX}, got {}",
        mat.graph.stats().total_entities
    );

    let pages = env
        .persistence
        .read_graph_pages(PROMPT_HASH, SESSION_ID)
        .await
        .expect("read spilled pages");
    assert!(
        !pages.is_empty(),
        "expected at least one durable graph page delta after paginated spill"
    );

    assert!(
        total > HOT_MAX,
        "Hermit should return enough berries to force spill (total={total}, hot_max={HOT_MAX})"
    );

    drop(mat);

    let program = "all = Berry\none = all.limit(1)\none";
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "graph_spill_filter",
        program,
    )
    .expect("compile plan");

    let live = run_plasm_comp(&es, &st, PROMPT_HASH, SESSION_ID, &bundle, true, None, None)
        .await
        .expect("live plan run");

    let one_step = live
        .return_steps
        .iter()
        .find(|s| s.node_id.as_deref() == Some("one"))
        .expect("one return step");
    assert_eq!(one_step.result.count, 1, "limit(1) on graph-backed surface");
    assert_eq!(
        one_step.result.entities.len(),
        1,
        "limit step should materialize one entity row"
    );
    let md = live.run_markdown.as_deref().unwrap_or("");
    assert!(
        !md.contains("(no results)"),
        "published markdown must include rehydrated rows, got: {md}"
    );

    let program_many = "all = Berry\nmany = all.limit(40)\nmany";
    let bundle_many = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "graph_spill_limit_many",
        program_many,
    )
    .expect("compile limit-many plan");

    let live_many = run_plasm_comp(
        &es,
        &st,
        PROMPT_HASH,
        SESSION_ID,
        &bundle_many,
        true,
        None,
        None,
    )
    .await
    .expect("live limit-many plan run");

    let many_step = live_many
        .return_steps
        .iter()
        .find(|s| s.node_id.as_deref() == Some("many"))
        .expect("many return step");
    let row_count = many_step.result.count;
    assert!(row_count > 0, "expected at least one berry row");
    assert!(
        row_count <= 40,
        "limit(40) should cap at 40 rows, got {row_count}"
    );

    use plasm_core::Value;

    let mut names = BTreeSet::new();
    for entity in &many_step.result.entities {
        if let Some(Value::String(name)) = entity
            .get_field("name")
            .map(plasm_core::TypedFieldValue::to_value)
        {
            names.insert(name);
        }
    }
    assert_eq!(
        names.len(),
        many_step.result.entities.len(),
        "limit-many rows must be unique by berry name (got {} rows, {} unique names)",
        many_step.result.entities.len(),
        names.len()
    );

    let md_many = live_many.run_markdown.as_deref().unwrap_or("");
    assert!(
        !md_many.contains("(no results)"),
        "limit-many markdown must include rows, got: {md_many}"
    );
}
