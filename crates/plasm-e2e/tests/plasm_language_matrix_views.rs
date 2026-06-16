//! View DAG conformance — preflight + catalog validation against `plasm_language_matrix_views`.
//!
//! Hermit is reserved for transport/decode regressions; view wiring lives here (fast, deterministic).

use plasm_compile::{validate_cgs_capability_templates, validate_cgs_views};
use plasm_core::QueryExpr;
use plasm_runtime::{
    preflight_view_query,
    view_test_support::{matrix_view_query, matrix_views_cgs, MATRIX_VIEW_PREFLIGHT_CASES},
    ViewAmbientContext,
};

#[test]
fn matrix_views_catalog_passes_static_validation() {
    let cgs = matrix_views_cgs();
    validate_cgs_capability_templates(&cgs).expect("CML templates");
    validate_cgs_views(&cgs).expect("views DAG");
}

#[test]
fn matrix_views_all_preflight() {
    let cgs = matrix_views_cgs();
    let ambient = ViewAmbientContext::default();
    for &(view_name, entity) in MATRIX_VIEW_PREFLIGHT_CASES {
        let query = matrix_view_query(entity);
        preflight_view_query(view_name, &query, &cgs, &ambient)
            .unwrap_or_else(|err| panic!("{view_name} preflight: {err}"));
    }
}

#[test]
fn matrix_views_missing_scope_preflight_errors() {
    let cgs = matrix_views_cgs();
    let query = QueryExpr::all("LangDigest");
    let err = preflight_view_query("lang_digest", &query, &cgs, &ViewAmbientContext::default())
        .expect_err("missing scope");
    assert!(err.to_string().contains("item_id"), "{err}");
}
