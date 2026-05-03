//! CGS + execute session wiring for the language-matrix Hermit base URL.

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
use plasm_core::{CgsContext, DomainExposureSession};
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
    let exp = DomainExposureSession::new(cgs.as_ref(), MATRIX_ENTRY_ID, wave);
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
    )
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
