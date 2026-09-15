//! Federated matrix fixtures for teaching-exposure replay and cross-pod rehydrate tests.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::CgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::{CgsContext, SymbolMap, TeachingExposureSession, CGS};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{
    apply_federate_exposure_wave, build_initial_exposure_wave, ExposureCatalogWave,
};
use crate::server_state::{CatalogBootstrap, PlasmHostState};

pub const LANGMATRIX_A: &str = "langmatrix_a";
pub const LANGMATRIX_B: &str = "langmatrix_b";

pub fn matrix_language_matrix_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

pub fn matrix_language_matrix_cgs() -> Arc<CGS> {
    Arc::new(load_schema_dir(&matrix_language_matrix_dir()).expect("plasm_language_matrix"))
}

pub fn matrix_federated_registry(cgs: Arc<CGS>) -> Arc<CgsRegistry> {
    Arc::new(CgsRegistry::from_pairs(vec![
        (
            LANGMATRIX_B.into(),
            "Langmatrix B".into(),
            vec![LANGMATRIX_B.into()],
            cgs.clone(),
        ),
        (
            LANGMATRIX_A.into(),
            "Langmatrix A".into(),
            vec![LANGMATRIX_A.into()],
            cgs.clone(),
        ),
    ]))
}

pub fn matrix_federated_host(cgs: Arc<CGS>) -> (PlasmHostState, Arc<CgsRegistry>) {
    let reg = matrix_federated_registry(cgs);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    let host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: reg.clone(),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    (host, reg)
}

/// Parallel entity/catalog rows for federated replay (lengths must match).
pub struct EntityCatalogPairing {
    pub entities: Vec<String>,
    pub catalog_entry_ids: Vec<String>,
}

impl EntityCatalogPairing {
    pub fn interleaved_federated_matrix() -> Self {
        Self {
            entities: vec!["LangItem".into(), "LangDetail".into(), "LangTag".into()],
            catalog_entry_ids: vec![
                LANGMATRIX_B.into(),
                LANGMATRIX_A.into(),
                LANGMATRIX_B.into(),
            ],
        }
    }
}

pub struct InterleavedFederatedFixture {
    pub contexts: IndexMap<String, Arc<CgsContext>>,
    pub cgs: Arc<CGS>,
    /// Live federate path: langmatrix_b LangItem → langmatrix_a LangDetail → langmatrix_b LangTag.
    pub live: TeachingExposureSession,
    pub pairing: EntityCatalogPairing,
}

/// Build contexts + live interleaved exposure for matrix federated replay tests.
pub fn interleaved_federated_matrix_fixture() -> InterleavedFederatedFixture {
    let cgs = matrix_language_matrix_cgs();
    let mut contexts = IndexMap::new();
    contexts.insert(
        LANGMATRIX_B.to_string(),
        Arc::new(CgsContext::entry(LANGMATRIX_B, cgs.clone())),
    );
    contexts.insert(
        LANGMATRIX_A.to_string(),
        Arc::new(CgsContext::entry(LANGMATRIX_A, cgs.clone())),
    );
    let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();

    let mut live = build_initial_exposure_wave(
        &contexts,
        &ExposureCatalogWave {
            entry_id: LANGMATRIX_B.to_string(),
            entities: vec!["LangItem".to_string()],
        },
    );
    apply_federate_exposure_wave(
        &mut live,
        &layers,
        &contexts,
        &ExposureCatalogWave {
            entry_id: LANGMATRIX_A.to_string(),
            entities: vec!["LangDetail".to_string()],
        },
    );
    apply_federate_exposure_wave(
        &mut live,
        &layers,
        &contexts,
        &ExposureCatalogWave {
            entry_id: LANGMATRIX_B.to_string(),
            entities: vec!["LangTag".to_string()],
        },
    );

    InterleavedFederatedFixture {
        contexts,
        cgs,
        live,
        pairing: EntityCatalogPairing::interleaved_federated_matrix(),
    }
}

/// Assert langmatrix_a `LangDetail` `e#` / `body` `p#` parity (primary federated numbering regression).
pub fn assert_langmatrix_a_langdetail_numbering_parity(live: &SymbolMap, other: &SymbolMap) {
    assert_eq!(
        other.entity_sym_for(LANGMATRIX_A, "LangDetail"),
        live.entity_sym_for(LANGMATRIX_A, "LangDetail"),
        "langmatrix_a LangDetail e# must match"
    );
    assert_eq!(
        other.ident_sym_entity_field_for(LANGMATRIX_A, "LangDetail", "body"),
        live.ident_sym_entity_field_for(LANGMATRIX_A, "LangDetail", "body"),
        "langmatrix_a LangDetail.body p# must match"
    );
}
