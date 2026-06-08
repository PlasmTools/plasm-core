//! Graph spill + hot trim + plan rehydrate (Hermit pokeapi_mini + file persistence).
//!
//! Requires `PLASM_GRAPH_CACHE_URL` and a low `PLASM_GRAPH_HOT_MAX_ENTITIES`; uses
//! [`serial_test::serial`] so env vars do not race other tests.

#![allow(dead_code)] // common::hermit re-exports petstore helpers unused by this binary.

mod common;

use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_agent::{
    execute_session::ExecuteSession,
    graph_page_spill_for_execute,
    http::{build_plasm_host_state, PlasmHostBootstrap},
    plasm_dag::compile_plasm_dag_to_plan,
    plasm_plan::{parse_plan_value, validate_plan_artifact},
    plasm_plan_run::run_validated_plasm_plan,
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
    session_graph_persistence::SessionGraphPersistence,
};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::{CgsContext, Expr, PromptPipelineConfig, QueryExpr, QueryPagination, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, StreamConsumeOpts,
};
use serial_test::serial;

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
        // SAFETY: test runs under `#[serial]`; no concurrent env mutation.
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
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("graph spill runtime");
            rt.block_on(graph_spill_bounded_hot_and_plan_filter_rehydrate_async());
        })
        .expect("spawn graph spill thread")
        .join()
        .expect("graph spill thread join");
}

#[serial]
async fn graph_spill_bounded_hot_and_plan_filter_rehydrate_async() {
    let env = GraphSpillEnv::install();
    let base = hermit::pokeapi_hermit_base_url().await.clone();
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
    let plan_json = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "graph_spill_filter",
        program,
    )
    .expect("compile plan");
    let plan = parse_plan_value(&plan_json).expect("parse plan");
    let validated = validate_plan_artifact(&plan).expect("validate plan");

    let live = run_validated_plasm_plan(
        &es,
        &st,
        PROMPT_HASH,
        SESSION_ID,
        &validated,
        true,
        None,
        None,
    )
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
}
