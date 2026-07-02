//! Shared GitHub execute-session builders for `plasm_dag` tests.

use super::super::*;
use crate::plasm_plan_run::symbol_map_for_plasm_surface_parse;
use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
use plasm_core::{load_schema, CgsContext, ExposureEntityKey, TeachingExposureSession, CGS};
use std::path::PathBuf;
use std::sync::{Arc, Once};

static GITHUB_FAST_LOAD: Once = Once::new();

fn enable_github_fast_load_for_tests() {
    GITHUB_FAST_LOAD.call_once(|| {
        // View-backed entities (IssueTriageContext, …) may lack teaching rows; structural load is enough for symbol resolution tests.
        std::env::set_var("PLASM_CGS_FAST_LOAD", "1");
    });
}

pub(super) fn github_cgs() -> Arc<CGS> {
    enable_github_fast_load_for_tests();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"))
}

pub(super) fn github_issue_label_session() -> ExecuteSession {
    enable_github_fast_load_for_tests();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
    let entities = ["Repository", "Issue", "Label"];
    let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &entities);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "github".into(),
        Arc::new(CgsContext::entry("github", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "github".into(),
        String::new(),
        String::new(),
        None,
        vec!["Repository".into(), "Issue".into(), "Label".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

pub(super) fn github_ranked_mutator_session(
    cgs: &Arc<CGS>,
    entities: &[&str],
    intent: &str,
    ranked: &[&str],
    mutator: &str,
) -> ExecuteSession {
    enable_github_fast_load_for_tests();
    let endpoints = entities
        .iter()
        .map(|e| ExposureEntityKey {
            entry_id: "github".into(),
            entity: plasm_core::EntityName::from(*e),
        })
        .collect::<Vec<_>>();
    let delta = derive_intent_exposure_surface_batch(
        cgs.as_ref(),
        "github",
        intent,
        &endpoints,
        &entities
            .iter()
            .map(|e| (*e).to_string())
            .collect::<Vec<_>>(),
        Some(&ranked.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()),
        ExposureSurfaceOptions {
            read_first_seeded: true,
        },
    );
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == mutator),
        "{mutator} must appear on ranked exposure delta"
    );
    let exp =
        TeachingExposureSession::new_with_intent_delta(cgs.as_ref(), "github", entities, delta);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "github".into(),
        Arc::new(CgsContext::entry("github", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "github".into(),
        String::new(),
        String::new(),
        None,
        entities.iter().map(|e| (*e).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

pub(super) fn compile_github_program(
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> serde_json::Value {
    compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        session,
        name,
        source,
    )
    .expect("compile")
}

pub(super) fn assert_compile_rejects_scalar_array_param(
    session: &ExecuteSession,
    plan_id: &str,
    source: &str,
) {
    let err = compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        session,
        plan_id,
        source,
    )
    .expect_err("scalar must not coerce to array");
    let msg = err.to_string();
    assert!(
        msg.contains("expected array") || msg.contains("array"),
        "expected array type error, got: {msg}"
    );
}

pub(super) fn assert_compile_rejects_unknown_cap_param(err: &str) {
    assert!(
        err.contains("is not an input parameter"),
        "expected invoke cap-param rejection ({err:?})"
    );
}

pub(super) fn assert_compile_rejects_query_filter_psym(err: &str) {
    assert!(
        err.contains("query filter") || err.contains("not a query filter symbol"),
        "expected query-filter rejection ({err:?})"
    );
}

pub(super) fn github_symbol_map(session: &ExecuteSession) -> Arc<dyn plasm_core::SymbolSession> {
    symbol_map_for_plasm_surface_parse(session, None)
}
