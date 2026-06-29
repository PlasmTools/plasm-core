//! Matrix-backed homograph `p#` projection regression (no `apis/github` coupling).

use super::super::*;
use super::test_support::github_symbol_map;
use crate::plasm_plan_run::evaluate_plasm_plan_dry;
use plasm_core::{CgsContext, PromptPipelineConfig, TeachingExposureSession};
use std::path::PathBuf;
use std::sync::Arc;

fn homograph_matrix_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("load plasm_language_matrix"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(
        cgs.as_ref(),
        "langmatrix",
        &["HomographRowA", "HomographRowB"],
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
        vec!["HomographRowA".into(), "HomographRowB".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

/// Shared value-domain fingerprint assigns one global `p#`; projection must still resolve per-entity wires.
#[test]
fn matrix_homograph_projection_resolves_entity_scoped_p_symbols() {
    let session = homograph_matrix_session();
    let map = github_symbol_map(&session);
    let row_a = map.entity_sym_for("langmatrix", "HomographRowA");
    let row_b = map.entity_sym_for("langmatrix", "HomographRowB");
    let p_headline = map.ident_sym_entity_field("HomographRowA", "headline");
    let p_caption = map.ident_sym_entity_field("HomographRowB", "caption");
    if p_headline != p_caption {
        // Distinct symbols — still verify each entity resolves its own wire.
    }
    let source_a = format!(
        "rows_a = {row_a}\nrows_a[{p_headline}]",
        row_a = row_a,
        p_headline = p_headline
    );
    let plan_a = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-homograph-a",
        &source_a,
    )
    .expect("HomographRowA projection");
    let return_a = plan_a["return"]["node"].as_str().expect("return");
    let node_a = plan_a["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == return_a)
        .expect("return node");
    assert!(
        node_a["compute"]["op"]["fields"]
            .as_object()
            .unwrap()
            .contains_key("headline")
    );

    let source_b = format!(
        "rows_b = {row_b}\nrows_b[{p_caption}]",
        row_b = row_b,
        p_caption = p_caption
    );
    let plan_b = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-homograph-b",
        &source_b,
    )
    .expect("HomographRowB projection");
    let return_b = plan_b["return"]["node"].as_str().expect("return");
    let node_b = plan_b["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == return_b)
        .expect("return node");
    let fields_b = node_b["compute"]["op"]["fields"].as_object().unwrap();
    assert!(fields_b.contains_key("caption"));
    assert!(!fields_b.contains_key("headline"));
    evaluate_plasm_plan_dry(&session, &plan_a).expect("dry a");
    evaluate_plasm_plan_dry(&session, &plan_b).expect("dry b");
}
