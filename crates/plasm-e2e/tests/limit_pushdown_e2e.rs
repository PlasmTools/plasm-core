//! Limit pushdown: `.limit(n)` on paginated queries bounds HTTP consumption (pokeapi_mini + Hermit).

#![allow(dead_code)]

mod common;

use std::path::Path;
use std::sync::Arc;

use plasm_agent::{
    execute_session::ExecuteSession,
    http::{build_plasm_host_state, PlasmHostBootstrap},
    plasm_compile::compile_plasm_program,
    plasm_plan_run::run_plasm_comp,
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
};
use plasm_core::{discovery::InMemoryCgsRegistry, CgsContext, PromptPipelineConfig, CGS};

use common::hermit;

const ENTRY_ID: &str = "default";
const PROMPT_HASH: &str = "limit_pushdown_ph";
const SESSION_ID: &str = "limit_pushdown_sess";

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

fn pokeapi_session(cgs: Arc<CGS>) -> ExecuteSession {
    let mut ctxs = indexmap::IndexMap::new();
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

#[test]
fn limit_pushdown_bounds_paginated_berry_query() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(limit_pushdown_bounds_paginated_berry_query_async());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn limit_pushdown_bounds_paginated_berry_query_async() {
    let _base = hermit::pokeapi_hermit_base_url().await.clone();
    let cgs = load_pokeapi_mini_cgs();
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        ENTRY_ID.into(),
        "PokeAPI Mini".into(),
        vec!["test".into()],
        cgs.clone(),
    )]));
    let engine = plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig {
        base_url: Some(_base.clone()),
        ..Default::default()
    })
    .expect("engine");
    let st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: plasm_runtime::ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    let es = pokeapi_session(cgs.clone());

    let program = "all = Berry\nlimited = all.limit(5)\nlimited";
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "limit_pushdown",
        program,
    )
    .expect("compile plan");

    let live = run_plasm_comp(
        &es,
        &st,
        PROMPT_HASH,
        SESSION_ID,
        &bundle,
        true,
        None,
        None,
    )
    .await
    .expect("live plan");

    let limited = live
        .return_steps
        .iter()
        .find(|s| s.node_id.as_deref() == Some("limited"))
        .expect("limited step");
    assert!(
        limited.result.count <= 5,
        "expected at most 5 rows, got {}",
        limited.result.count
    );
    assert!(
        limited.result.entities.len() <= 5,
        "expected at most 5 entity rows, got {}",
        limited.result.entities.len()
    );
}
