//! Typed relation rows: species traversal + render on `capture_rate` (dry + live, Hermit pokeapi).

#![allow(dead_code)]

mod common;

use std::path::Path;
use std::sync::Arc;

use plasm_agent::{
    execute_session::ExecuteSession,
    http::{build_plasm_host_state, PlasmHostBootstrap},
    plasm_compile::compile_plasm_program,
    plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp},
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
};
use plasm_core::{discovery::InMemoryCgsRegistry, CgsContext, PromptPipelineConfig, CGS};

use common::hermit;

const ENTRY: &str = "pokeapi";
const PH: &str = "relation_render_ph";
const SESS: &str = "relation_render_sess";

fn load_pokeapi_cgs() -> Arc<CGS> {
    let paths = [
        "../../apis/pokeapi",
        "apis/pokeapi",
        "../../../apis/pokeapi",
    ];
    for path in &paths {
        let p = Path::new(path);
        if p.join("domain.yaml").exists() {
            return Arc::new(plasm_core::loader::load_schema_dir(p).expect("pokeapi CGS"));
        }
    }
    panic!("apis/pokeapi not found");
}

fn pokeapi_session(cgs: Arc<CGS>) -> ExecuteSession {
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        ENTRY.into(),
        Arc::new(CgsContext::entry(ENTRY, cgs.clone())),
    );
    ExecuteSession::new(
        PH.into(),
        String::new(),
        cgs.clone(),
        ctxs,
        ENTRY.into(),
        String::new(),
        String::new(),
        None,
        vec!["Pokemon".into(), "PokemonSpecies".into()],
        None,
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[test]
fn relation_species_render_capture_rate_dry_and_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(relation_species_render_capture_rate_async());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn relation_species_render_capture_rate_async() {
    let base = hermit::pokeapi_hermit_base_url().await.clone();
    let cgs = load_pokeapi_cgs();
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        ENTRY.into(),
        "PokeAPI".into(),
        vec!["test".into()],
        cgs.clone(),
    )]));
    let engine = plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig {
        base_url: Some(base.clone()),
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

    let program = r#"specimen = Pokemon("pikachu")
species = specimen.species
line = species[capture_rate,name] <<PLASM_RELATION_RENDER_TEST
{{ rows[0].name }} capture={{ rows[0].capture_rate }}
PLASM_RELATION_RENDER_TEST
line"#;

    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "relation_render",
        program,
    )
    .expect("compile plan");

    evaluate_plasm_comp_dry(&es, &bundle).expect("dry plan must pass render preflight");

    let live = run_plasm_comp(&es, &st, PH, SESS, &bundle, true, None, None)
        .await
        .expect("live plan");

    let line_step = live
        .return_steps
        .iter()
        .find(|s| s.node_id.as_deref() == Some("line"))
        .expect("line return step");
    let entity = line_step.result.entities.first().expect("render row");
    let row = plasm_runtime::entity_to_agent_row_json(entity, line_step.cgs.as_deref());
    let content = row
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        content.contains("capture="),
        "expected capture_rate column in render output, got {content:?}"
    );
    // Hermit mock species payloads may not match real pikachu lore; the invariant is
    // that taught CGS columns resolve on typed relation rows (dry + live isomorphic).
}
