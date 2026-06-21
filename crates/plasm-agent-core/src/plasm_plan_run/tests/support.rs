use super::super::*;
use plasm_core::load_schema;
use plasm_core::CgsContext;
use std::path::PathBuf;

pub(super) fn test_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "acme".into(),
        Arc::new(CgsContext::entry("acme", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product", "Category"]);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "acme".into(),
        String::new(),
        String::new(),
        None,
        vec!["Product".into(), "Category".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

pub(super) fn duplicate_product_create_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs_base = load_schema(&root.join("tests/fixtures/scoped_create_tiny"))
        .expect("load scoped_create_tiny");
    let cgs_acme = Arc::new({
        let mut c = cgs_base.clone();
        c.entry_id = Some("acme".into());
        c
    });
    let cgs_other = Arc::new({
        let mut c = cgs_base;
        c.entry_id = Some("other".into());
        c
    });
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "acme".into(),
        Arc::new(CgsContext::entry("acme", cgs_acme.clone())),
    );
    ctxs.insert(
        "other".into(),
        Arc::new(CgsContext::entry("other", cgs_other.clone())),
    );
    let mut exp = TeachingExposureSession::new(cgs_acme.as_ref(), "acme", &["Product"]);
    exp.expose_entities(
        &[cgs_acme.as_ref(), cgs_other.as_ref()],
        cgs_other.clone(),
        "other",
        &["Product"],
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs_acme.clone(),
        ctxs,
        "acme".into(),
        String::new(),
        String::new(),
        None,
        vec!["Product".into()],
        Some(exp),
        None,
        cgs_acme.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[test]
fn cmp_json_sort_values_orders_multi_digit_numbers_numerically() {
    use std::cmp::Ordering;
    let n87 = serde_json::json!(87);
    let n300 = serde_json::json!(300);
    assert_eq!(
        cmp_json_sort_values(Some(&n87), Some(&n300)),
        Ordering::Less
    );
    let s87 = serde_json::json!("87");
    let s300 = serde_json::json!("300");
    assert_eq!(
        cmp_json_sort_values(Some(&s87), Some(&s300)),
        Ordering::Less
    );
}

#[test]
fn singleton_input_zero_row_error_is_actionable() {
    let err = singleton_input_row_count_error("src", "_", 0, "staged expression rendering");
    assert!(err.contains("zero rows"), "{err}");
    assert!(err.contains("not a Plasm syntax error"), "{err}");
    assert!(err.contains("branch around empty results"), "{err}");
}

#[test]
fn singleton_input_multi_row_error_mentions_ambiguity_remedy() {
    let err = singleton_input_row_count_error("src", "_", 2, "staged expression rendering");
    assert!(err.contains("2 rows"), "{err}");
    assert!(err.contains("make the source unique"), "{err}");
    assert!(err.contains(".singleton()"), "{err}");
}

#[test]
fn cmp_json_sort_values_string_collates_non_numeric_strings_lexically() {
    use std::cmp::Ordering;
    let apple = serde_json::json!("apple");
    let banana = serde_json::json!("banana");
    assert_eq!(
        cmp_json_sort_values(Some(&apple), Some(&banana)),
        Ordering::Less
    );
}

/// Regression: `.sort(score)` must not stringify numbers and compare lexicographically (where
/// `87` sorts after `300`). Keeps parity with [`eval_compute`] `ComputeOp::Sort` staging.
#[test]
fn plan_sort_compute_orders_integer_scores_numerically() {
    let key = FieldPath::from_dotted("score").expect("score path");
    let mut rows = [
        serde_json::json!({"id": "n300", "score": 300}),
        serde_json::json!({"id": "n87", "score": 87}),
        serde_json::json!({"id": "n100", "score": 100}),
    ];
    rows.sort_by(|a, b| cmp_json_sort_values(value_at_path(a, &key), value_at_path(b, &key)));
    assert_eq!(rows[0]["id"], "n87");
    assert_eq!(rows[1]["id"], "n100");
    assert_eq!(rows[2]["id"], "n300");

    rows.reverse();
    assert_eq!(rows[0]["id"], "n300");
    assert_eq!(rows[1]["id"], "n100");
    assert_eq!(rows[2]["id"], "n87");
}
