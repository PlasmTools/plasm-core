//! Regression: view DAG nodes with literal param binds must compile optional CML
//! `if: {exists: …}` body/query fields (see `plasm-cml::wire_normalize`).

use plasm_runtime::{
    preflight_view_query,
    view_test_support::{matrix_view_query, matrix_views_cgs},
    ViewAmbientContext,
};

#[test]
fn matrix_lang_tag_filter_demo_literal_label_bind_preflight() {
    let cgs = matrix_views_cgs();
    let query = matrix_view_query("LangTagFilterDemo");
    preflight_view_query(
        "lang_tag_filter_demo",
        &query,
        &cgs,
        &ViewAmbientContext::default(),
    )
    .unwrap_or_else(|e| panic!("lang_tag_filter_demo preflight: {e}"));
}
