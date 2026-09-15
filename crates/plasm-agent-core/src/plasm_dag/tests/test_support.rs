//! Shared language-matrix execute-session builders for `plasm_dag` tests.

use super::super::*;
use crate::plasm_plan_run::symbol_map_for_plasm_surface_parse;

use plasm_core::{load_schema, CgsContext, TeachingExposureSession, CGS};
use std::path::PathBuf;
use std::sync::Arc;

pub(super) fn matrix_cgs() -> Arc<CGS> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cgs = load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
        .expect("load plasm_language_matrix");
    cgs.bind_registry_entry_id("langmatrix");
    Arc::new(cgs)
}

pub(super) fn langitem_tag_session() -> ExecuteSession {
    let cgs = matrix_cgs();
    let entities = ["LangItem", "LangTag"];
    let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &entities);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into(), "LangTag".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

pub(super) fn langitem_ranked_mutator_session(
    cgs: &Arc<CGS>,
    entities: &[&str],
    _intent: &str,
    _ranked: &[&str],
    mutator: &str,
) -> ExecuteSession {
    let delta = plasm_core::capability_exposure::explicit_entity_capability_surface(
        cgs.as_ref(),
        "langmatrix",
        &entities
            .iter()
            .map(|e| (*e).to_string())
            .collect::<Vec<_>>(),
    )
    .expect("explicit fixture capability exposure");
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == mutator),
        "{mutator} must appear on ranked exposure delta"
    );
    let exp =
        TeachingExposureSession::new_with_intent_delta(cgs.as_ref(), "langmatrix", entities, delta);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        entities.iter().map(|e| (*e).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

pub(super) fn compile_matrix_program(
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

pub(super) fn matrix_symbol_map(session: &ExecuteSession) -> Arc<dyn plasm_core::SymbolSession> {
    symbol_map_for_plasm_surface_parse(session, None)
}
