//! Federated matrix fixtures for teaching-exposure replay and cross-pod rehydrate tests.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::MutatorAdmit;
use plasm_core::{CgsContext, SymbolMap, TeachingExposureSession, CGS};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{
    apply_federate_exposure_wave, build_initial_exposure_wave, ExposureCatalogWave,
};
use crate::server_state::{CatalogBootstrap, PlasmHostState};

pub fn matrix_language_matrix_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

pub fn matrix_language_matrix_cgs() -> Arc<CGS> {
    Arc::new(load_schema_dir(&matrix_language_matrix_dir()).expect("plasm_language_matrix"))
}

pub fn matrix_federated_registry(cgs: Arc<CGS>) -> Arc<InMemoryCgsRegistry> {
    Arc::new(InMemoryCgsRegistry::from_pairs(vec![
        (
            "linear".into(),
            "Linear".into(),
            vec!["linear".into()],
            cgs.clone(),
        ),
        (
            "github".into(),
            "GitHub".into(),
            vec!["github".into()],
            cgs.clone(),
        ),
    ]))
}

pub fn matrix_federated_host(cgs: Arc<CGS>) -> (PlasmHostState, Arc<InMemoryCgsRegistry>) {
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
            catalog_entry_ids: vec!["linear".into(), "github".into(), "linear".into()],
        }
    }
}

pub struct InterleavedFederatedFixture {
    pub contexts: IndexMap<String, Arc<CgsContext>>,
    pub cgs: Arc<CGS>,
    /// Live federate path: linear LangItem → github LangDetail → linear LangTag.
    pub live: TeachingExposureSession,
    pub pairing: EntityCatalogPairing,
}

/// Build contexts + live interleaved exposure for matrix federated replay tests.
pub fn interleaved_federated_matrix_fixture() -> InterleavedFederatedFixture {
    let cgs = matrix_language_matrix_cgs();
    let mut contexts = IndexMap::new();
    contexts.insert(
        "linear".to_string(),
        Arc::new(CgsContext::entry("linear", cgs.clone())),
    );
    contexts.insert(
        "github".to_string(),
        Arc::new(CgsContext::entry("github", cgs.clone())),
    );
    let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();

    let mut live = build_initial_exposure_wave(
        &contexts,
        &ExposureCatalogWave {
            entry_id: "linear".to_string(),
            entities: vec!["LangItem".to_string()],
            mutator_admit: MutatorAdmit::IntentOnly,
        },
        None,
        None,
    );
    apply_federate_exposure_wave(
        &mut live,
        &layers,
        &contexts,
        &ExposureCatalogWave {
            entry_id: "github".to_string(),
            entities: vec!["LangDetail".to_string()],
            mutator_admit: MutatorAdmit::IntentOnly,
        },
        None,
        None,
    );
    apply_federate_exposure_wave(
        &mut live,
        &layers,
        &contexts,
        &ExposureCatalogWave {
            entry_id: "linear".to_string(),
            entities: vec!["LangTag".to_string()],
            mutator_admit: MutatorAdmit::IntentOnly,
        },
        None,
        None,
    );

    InterleavedFederatedFixture {
        contexts,
        cgs,
        live,
        pairing: EntityCatalogPairing::interleaved_federated_matrix(),
    }
}

/// Assert github `LangDetail` `e#` / `body` `p#` parity (primary federated numbering regression).
pub fn assert_github_langdetail_numbering_parity(live: &SymbolMap, other: &SymbolMap) {
    assert_eq!(
        other.entity_sym_for("github", "LangDetail"),
        live.entity_sym_for("github", "LangDetail"),
        "github LangDetail e# must match"
    );
    assert_eq!(
        other.ident_sym_entity_field_for("github", "LangDetail", "body"),
        live.ident_sym_entity_field_for("github", "LangDetail", "body"),
        "github LangDetail.body p# must match"
    );
}
