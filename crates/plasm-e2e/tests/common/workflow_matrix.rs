//! CGS + federated host wiring for workflow-matrix Hermit tests.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_agent::{
    http::{build_plasm_host_state, PlasmHostBootstrap},
    run_artifacts::RunArtifactStore,
    server_state::CatalogBootstrap,
};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::{CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionEngine, ExecutionMode};

pub const CATALOG_A: &str = "catalog_a";
pub const CATALOG_B: &str = "catalog_b";

pub fn workflow_matrix_schema_dir() -> PathBuf {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        crate_root.join("../../../fixtures/schemas/workflow_matrix"),
        crate_root.join("../../../../fixtures/schemas/workflow_matrix"),
        crate_root.join("../../fixtures/schemas/workflow_matrix"),
        crate_root.join("fixtures/schemas/workflow_matrix"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    panic!(
        "fixtures/schemas/workflow_matrix not found (tried {:?})",
        candidates
    );
}

pub fn load_workflow_matrix_cgs() -> Arc<plasm_core::CGS> {
    let dir = workflow_matrix_schema_dir();
    Arc::new(
        plasm_core::loader::load_schema_dir(&dir).unwrap_or_else(|e| {
            panic!("load workflow_matrix CGS from {}: {e}", dir.display());
        }),
    )
}

pub fn workflow_federated_host_state(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![
        (
            CATALOG_A.into(),
            "Catalog A (workflow matrix)".into(),
            vec!["workflow".into()],
            cgs.clone(),
        ),
        (
            CATALOG_B.into(),
            "Catalog B (workflow matrix)".into(),
            vec!["workflow".into()],
            cgs.clone(),
        ),
    ]));
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
                incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

#[allow(dead_code)]
pub fn workflow_federated_session(
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::execute_session::ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        CATALOG_A.into(),
        Arc::new(CgsContext::entry(CATALOG_A, cgs.clone())),
    );
    ctxs.insert(
        CATALOG_B.into(),
        Arc::new(CgsContext::entry(CATALOG_B, cgs.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs.as_ref(), cgs.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs.as_ref(), CATALOG_A, &["WorkItem"]);
    exp.expose_entities(&layers, cgs.clone(), CATALOG_B, &["WorkItem"]);
    plasm_agent::execute_session::ExecuteSession::new(
        "wf_ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        CATALOG_A.into(),
        String::new(),
        String::new(),
        None,
        vec!["WorkItem".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}
