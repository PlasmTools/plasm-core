//! Shared `plasm_language_matrix_views` fixture helpers (runtime tests + e2e conformance).

use indexmap::IndexMap;
use plasm_core::loader::load_schema_dir;
use plasm_core::{Predicate, QueryExpr, Value, CGS};

pub const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/schemas/plasm_language_matrix_views"
);

/// Every composed view in the matrix views extension fixture.
pub const MATRIX_VIEW_PREFLIGHT_CASES: &[(&str, &str)] = &[
    ("lang_digest", "LangDigest"),
    ("lang_triage_context", "LangTriageContext"),
    ("lang_item_link", "LangItemLink"),
    ("lang_owner_filter_demo", "LangOwnerFilterDemo"),
];

pub fn matrix_views_cgs() -> CGS {
    load_schema_dir(std::path::Path::new(FIXTURE_DIR)).expect("matrix views cgs")
}

pub fn matrix_view_query(entity: &str) -> QueryExpr {
    QueryExpr::filtered(
        entity,
        Predicate::eq("item_id", Value::String("item-1".into())),
    )
}

pub fn lang_digest_scope() -> IndexMap<String, Value> {
    IndexMap::from([("item_id".into(), Value::String("item-1".into()))])
}
