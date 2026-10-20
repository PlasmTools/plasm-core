//! Shared `plasm_language_matrix_views` fixture helpers (runtime tests + e2e conformance).

use indexmap::IndexMap;
use plasm_core::loader::load_schema_dir;
use plasm_core::{Predicate, QueryExpr, Value, CGS};

pub fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("test runner must set CARGO_MANIFEST_DIR"),
    )
    .join("../../fixtures/schemas/plasm_language_matrix_views")
}

/// Every composed view in the matrix views extension fixture.
pub const MATRIX_VIEW_PREFLIGHT_CASES: &[(&str, &str)] = &[
    ("lang_digest", "LangDigest"),
    ("lang_triage_context", "LangTriageContext"),
    ("lang_item_link", "LangItemLink"),
    ("lang_owner_filter_demo", "LangOwnerFilterDemo"),
    ("lang_tag_filter_demo", "LangTagFilterDemo"),
    ("lang_work_snapshot", "LangWorkSnapshot"),
    ("lang_work_snapshot_empty", "LangWorkSnapshotEmpty"),
];

pub fn matrix_views_cgs() -> CGS {
    load_schema_dir(&fixture_dir()).expect("matrix views cgs")
}

pub fn matrix_view_query(entity: &str) -> QueryExpr {
    if matches!(entity, "LangWorkSnapshot" | "LangWorkSnapshotEmpty") {
        return QueryExpr::all(entity);
    }
    QueryExpr::filtered(
        entity,
        Predicate::eq("item_id", Value::String("item-1".into())),
    )
}

pub fn lang_digest_scope() -> IndexMap<String, Value> {
    IndexMap::from([("item_id".into(), Value::String("item-1".into()))])
}
