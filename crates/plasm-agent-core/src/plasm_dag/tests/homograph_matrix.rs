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
    let p_headline = map.ident_sym_entity_field_for("langmatrix", "HomographRowA", "headline");
    let p_caption = map.ident_sym_entity_field_for("langmatrix", "HomographRowB", "caption");
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
    assert!(node_a["compute"]["op"]["fields"]
        .as_object()
        .unwrap()
        .contains_key("headline"));

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

fn langitem_create_query_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("load plasm_language_matrix"),
    );
    let endpoints = ["LangItem"]
        .iter()
        .map(|e| plasm_core::ExposureEntityKey {
            entry_id: "langmatrix".into(),
            entity: plasm_core::EntityName::from(*e),
        })
        .collect::<Vec<_>>();
    let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
        cgs.as_ref(),
        "langmatrix",
        "create and list lang items",
        &endpoints,
        &["LangItem".to_string()],
        Some(&["langitem_create".to_string(), "langitem_query".to_string()]),
        plasm_core::discovery::ExposureSurfaceOptions {
            read_first_seeded: true,
        },
    );
    let exp = TeachingExposureSession::new_with_intent_delta(
        cgs.as_ref(),
        "langmatrix",
        &["LangItem"],
        delta,
    );
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
        vec!["LangItem".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

/// Session `m#` for create must bind `langitem_create`, never validate invoke args against query caps.
#[test]
fn matrix_create_m_never_validates_against_query_cap() {
    let session = langitem_create_query_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let query_m = map.method_sym_for("langmatrix", "LangItem", "langitem_query");
    assert_ne!(create_m, query_m, "create and query must have distinct m# tokens");
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_title}="hello")
created"#,
        item_e = item_e,
        create_m = create_m,
        p_title = p_title,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-create-m-invoke",
        &source,
    )
    .expect("create invoke must compile");
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    assert_eq!(created["kind"], "create");
    let ir = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR");
    assert!(
        ir.to_string().contains("langitem_create"),
        "m# must dispatch to create cap, not query: {ir}"
    );
    assert!(
        !ir.to_string().contains("langitem_query"),
        "must not resolve query cap for create m#: {ir}"
    );
}
