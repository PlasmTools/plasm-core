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
use plasm_core::discovery::CgsRegistry;
use plasm_core::{CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionEngine, ExecutionMode};

pub const MATRIX_ENTRY_ID: &str = "langmatrix";
/// Federated primary stamp (same fixture CGS, distinct registry id).
pub const MATRIX_FED_A: &str = "langmatrix_a";
/// Federated secondary stamp (same fixture CGS, distinct registry id).
pub const MATRIX_FED_B: &str = "langmatrix_b";

/// Clone a fixture [`CGS`] and stamp registry `entry_id` for federated parser/layer tests.
pub fn cgs_with_registry_entry_id(cgs: &plasm_core::CGS, entry_id: &str) -> plasm_core::CGS {
    let mut out = cgs.clone();
    out.bind_registry_entry_id(entry_id);
    out
}

/// Dual-catalog session: AuthSession on both federated stamps; secured notes on B; secured groups on A
/// (two logins → two Bearer surfaces in one program).
#[allow(dead_code)]
pub fn matrix_federated_auth_session_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let cgs_a = Arc::new(cgs_with_registry_entry_id(cgs.as_ref(), MATRIX_FED_A));
    let cgs_b = Arc::new(cgs_with_registry_entry_id(cgs.as_ref(), MATRIX_FED_B));
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        MATRIX_FED_A.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_A, cgs_a.clone())),
    );
    ctxs.insert(
        MATRIX_FED_B.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_B, cgs_b.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_a.as_ref(), cgs_b.as_ref()];
    let mut exp = TeachingExposureSession::new(
        cgs_a.as_ref(),
        MATRIX_FED_A,
        &["LangAuthSession", "LangSecuredGroup"],
    );
    exp.expose_entities(
        &layers,
        cgs_b.clone(),
        MATRIX_FED_B,
        &["LangAuthSession", "LangSecuredNote"],
    );
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs_a.clone(),
        ctxs,
        MATRIX_FED_A.into(),
        String::new(),
        String::new(),
        None,
        vec![
            "LangAuthSession".into(),
            "LangSecuredGroup".into(),
            "LangSecuredNote".into(),
        ],
        Some(exp),
        None,
        cgs_a.catalog_cgs_hash_hex(),
        None,
    )
}

#[allow(dead_code)] // Shared fixture helper; not every integration binary uses it.
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

#[allow(dead_code)] // Shared fixture helper; not every integration binary uses it.
pub fn load_language_matrix_cgs() -> Arc<plasm_core::CGS> {
    let dir = language_matrix_schema_dir();
    Arc::new(
        plasm_core::loader::load_schema_dir(&dir).unwrap_or_else(|e| {
            panic!("load plasm_language_matrix CGS from {}: {e}", dir.display());
        }),
    )
}

#[allow(dead_code)] // shared helper; not every matrix e2e binary uses the default wave session
pub fn matrix_execute_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        MATRIX_ENTRY_ID.into(),
        Arc::new(CgsContext::entry(MATRIX_ENTRY_ID, cgs.clone())),
    );
    let wave: &[&str] = &["LangItem", "LangLine", "LangTag", "LangOffer"];
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
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

/// Same wire entity (`LangItem`) in `langmatrix_a` and `langmatrix_b` — distinct session `e1` / `e2`.
#[allow(dead_code)]
pub fn matrix_federated_duplicate_entity_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let cgs_a = Arc::new(cgs_with_registry_entry_id(cgs.as_ref(), MATRIX_FED_A));
    let cgs_b = Arc::new(cgs_with_registry_entry_id(cgs.as_ref(), MATRIX_FED_B));
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        MATRIX_FED_A.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_A, cgs_a.clone())),
    );
    ctxs.insert(
        MATRIX_FED_B.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_B, cgs_b.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_a.as_ref(), cgs_b.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_a.as_ref(), MATRIX_FED_A, &["LangItem"]);
    exp.expose_entities(&layers, cgs_b.clone(), MATRIX_FED_B, &["LangItem"]);
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs_a.clone(),
        ctxs,
        MATRIX_FED_A.into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into()],
        Some(exp),
        None,
        cgs_a.catalog_cgs_hash_hex(),
        None,
    )
}

#[allow(dead_code)]
pub fn matrix_federated_duplicate_entity_host_state(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(CgsRegistry::from_pairs(vec![
        (
            MATRIX_FED_A.into(),
            "Language matrix A (federated duplicate LangItem)".into(),
            vec!["matrix".into()],
            cgs.clone(),
        ),
        (
            MATRIX_FED_B.into(),
            "Language matrix B (federated duplicate LangItem)".into(),
            vec!["matrix".into()],
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
pub fn matrix_federated_relation_target_session(
    cgs_primary: Arc<plasm_core::CGS>,
    cgs_secondary: Arc<plasm_core::CGS>,
) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        MATRIX_FED_A.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_A, cgs_primary.clone())),
    );
    ctxs.insert(
        MATRIX_FED_B.into(),
        Arc::new(CgsContext::entry(MATRIX_FED_B, cgs_secondary.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_primary.as_ref(), cgs_secondary.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_primary.as_ref(), MATRIX_FED_A, &["LangLine"]);
    exp.expose_entities(&layers, cgs_secondary.clone(), MATRIX_FED_B, &["LangItem"]);
    let wave: &[&str] = &["LangItem", "LangLine"];
    ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs_primary.clone(),
        ctxs,
        MATRIX_FED_A.into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        cgs_primary.catalog_cgs_hash_hex(),
        None,
    )
}

#[allow(dead_code)]
pub fn matrix_federated_host_state(
    engine: ExecutionEngine,
    cgs_primary: Arc<plasm_core::CGS>,
    cgs_secondary: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(CgsRegistry::from_pairs(vec![
        (
            MATRIX_FED_A.into(),
            "Language matrix A (federated primary)".into(),
            vec!["matrix".into()],
            cgs_primary,
        ),
        (
            MATRIX_FED_B.into(),
            "Language matrix B (federated secondary)".into(),
            vec!["matrix".into()],
            cgs_secondary,
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

pub fn matrix_host_state(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(CgsRegistry::from_pairs(vec![(
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
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}
