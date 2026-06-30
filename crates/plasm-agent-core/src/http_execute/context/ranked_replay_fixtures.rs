//! Shared matrix / GitHub exposure fixtures for ranked-replay conformance tests.

#![allow(dead_code)]

use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
use plasm_core::loader::{load_schema, load_schema_dir};
use plasm_core::{ExposureEntityKey, TeachingExposureSession, CGS};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn load_matrix_cgs() -> CGS {
    load_schema_dir(&fixture_root().join("../../fixtures/schemas/plasm_language_matrix"))
        .expect("plasm_language_matrix")
}

pub(crate) fn load_github_cgs() -> CGS {
    load_schema(&fixture_root().join("../../apis/github")).expect("github")
}

pub(crate) fn matrix_langitem_endpoints() -> Vec<ExposureEntityKey> {
    vec![ExposureEntityKey {
        entry_id: "matrix".into(),
        entity: plasm_core::EntityName::from("LangItem"),
    }]
}

pub(crate) fn github_issue_repo_endpoints() -> Vec<ExposureEntityKey> {
    ["Repository", "Issue"]
        .iter()
        .map(|e| ExposureEntityKey {
            entry_id: "github".into(),
            entity: plasm_core::EntityName::from(*e),
        })
        .collect()
}

pub(crate) fn matrix_exp_with_intent(
    intent: &str,
    ranked: Option<&[String]>,
    read_first: bool,
) -> TeachingExposureSession {
    let cgs = load_matrix_cgs();
    let entities = ["LangItem"];
    let endpoints = matrix_langitem_endpoints();
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        intent,
        &endpoints,
        &entities
            .iter()
            .map(|e| (*e).to_string())
            .collect::<Vec<_>>(),
        ranked,
        ExposureSurfaceOptions {
            read_first_seeded: read_first,
        },
    );
    TeachingExposureSession::new_with_intent_delta(&cgs, "matrix", &entities, delta)
}

pub(crate) fn github_exp_with_intent(
    intent: &str,
    ranked: Option<&[String]>,
    read_first: bool,
) -> TeachingExposureSession {
    let cgs = load_github_cgs();
    let entities = vec!["Repository".to_string(), "Issue".to_string()];
    let endpoints = github_issue_repo_endpoints();
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "github",
        intent,
        &endpoints,
        &entities,
        ranked,
        ExposureSurfaceOptions {
            read_first_seeded: read_first,
        },
    );
    TeachingExposureSession::new_with_intent_delta(&cgs, "github", &["Repository", "Issue"], delta)
}

pub(crate) fn matrix_cgs_arc() -> Arc<CGS> {
    Arc::new(load_matrix_cgs())
}

pub(crate) fn github_cgs_arc() -> Arc<CGS> {
    Arc::new(load_github_cgs())
}
