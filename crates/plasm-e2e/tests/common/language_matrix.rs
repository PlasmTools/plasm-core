//! CGS + execute session wiring for the language-matrix Hermit base URL.
#![allow(dead_code)] // federated matrix helpers kept for upcoming witness cases

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_agent::{
    execute_session::ExecuteSession,
    http::{build_plasm_host_state, PlasmHostBootstrap},
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::{CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionEngine, ExecutionMode};

pub const MATRIX_ENTRY_ID: &str = "langmatrix";

pub fn language_matrix_schema_dir() -> PathBuf {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        crate_root.join("../../fixtures/schemas/plasm_language_matrix"),
        crate_root.join("fixtures/schemas/plasm_language_matrix"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    panic!(
        "fixtures/schemas/plasm_language_matrix not found (tried {:?})",
        candidates
    );
}

pub fn load_language_matrix_cgs() -> Arc<plasm_core::CGS> {
    let dir = language_matrix_schema_dir();
    Arc::new(
        plasm_core::loader::load_schema_dir(&dir).unwrap_or_else(|e| {
            panic!("load plasm_language_matrix CGS from {}: {e}", dir.display());
        }),
    )
}

pub fn matrix_execute_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        MATRIX_ENTRY_ID.into(),
        Arc::new(CgsContext::entry(MATRIX_ENTRY_ID, cgs.clone())),
    );
    let wave: &[&str] = &["LangItem", "LangLine", "LangTag"];
    let exp = TeachingExposureSession::new(cgs.as_ref(), MATRIX_ENTRY_ID, wave);
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        MATRIX_ENTRY_ID.into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

/// Same wire entity (`LangItem`) in `github` and `linear` catalogs — distinct session `e1` / `e2`.
pub fn matrix_federated_duplicate_entity_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "github".into(),
        Arc::new(CgsContext::entry("github", cgs.clone())),
    );
    ctxs.insert(
        "linear".into(),
        Arc::new(CgsContext::entry("linear", cgs.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs.as_ref(), cgs.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
    exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        "github".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into()],
        Some(exp),
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

pub fn matrix_federated_duplicate_entity_host_state(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![
        (
            "github".into(),
            "GitHub (matrix federated duplicate LangItem)".into(),
            vec!["matrix".into()],
            cgs.clone(),
        ),
        (
            "linear".into(),
            "Linear (matrix federated duplicate LangItem)".into(),
            vec!["matrix".into()],
            cgs.clone(),
        ),
    ]));
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

pub fn matrix_federated_relation_target_session(
    cgs_primary: Arc<plasm_core::CGS>,
    cgs_secondary: Arc<plasm_core::CGS>,
) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "linear".into(),
        Arc::new(CgsContext::entry("linear", cgs_primary.clone())),
    );
    ctxs.insert(
        "pokeapi".into(),
        Arc::new(CgsContext::entry("pokeapi", cgs_secondary.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_primary.as_ref(), cgs_secondary.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_primary.as_ref(), "linear", &["LangLine"]);
    exp.expose_entities(&layers, cgs_secondary.clone(), "pokeapi", &["LangItem"]);
    let wave: &[&str] = &["LangItem", "LangLine"];
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs_primary.clone(),
        ctxs,
        "linear".into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        None,
        cgs_primary.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

pub fn matrix_federated_host_state(
    engine: ExecutionEngine,
    cgs_primary: Arc<plasm_core::CGS>,
    cgs_secondary: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![
        (
            "linear".into(),
            "Linear (matrix federated primary)".into(),
            vec!["matrix".into()],
            cgs_primary,
        ),
        (
            "pokeapi".into(),
            "Pokeapi (matrix federated secondary)".into(),
            vec!["matrix".into()],
            cgs_secondary,
        ),
    ]));
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

pub fn matrix_host_state(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        MATRIX_ENTRY_ID.into(),
        "Plasm Language Matrix".into(),
        vec!["matrix".into()],
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
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}
