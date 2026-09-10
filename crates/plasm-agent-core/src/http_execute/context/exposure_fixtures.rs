//! Abstract exposure fixtures for symbol and session tests.

use plasm_core::loader::load_schema_dir;
use plasm_core::{TeachingExposureSession, CGS};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn load_matrix_cgs() -> CGS {
    load_schema_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .expect("plasm_language_matrix")
}

pub(crate) fn matrix_exp_explicit() -> TeachingExposureSession {
    let cgs = load_matrix_cgs();
    let delta = plasm_core::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        "matrix",
        &["LangItem".into()],
    )
    .expect("explicit matrix capability exposure");
    TeachingExposureSession::new_with_intent_delta(&cgs, "matrix", &["LangItem"], delta)
}

pub(crate) fn matrix_cgs_arc() -> Arc<CGS> {
    Arc::new(load_matrix_cgs())
}
