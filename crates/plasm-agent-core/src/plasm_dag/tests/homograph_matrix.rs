//! Matrix-backed homograph `p#` projection regression (no `apis/github` coupling).

use super::super::*;
use super::test_support::assert_compile_rejects_query_filter_psym;
use super::test_support::assert_compile_rejects_scalar_array_param;
use super::test_support::assert_compile_rejects_unknown_cap_param;
use super::test_support::github_symbol_map;
use crate::plasm_plan_run::evaluate_plasm_plan_dry;
use plasm_core::{CgsContext, PromptPipelineConfig, SymbolMap, TeachingExposureSession};
use plasm_core::MutatorAdmit;
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
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
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
    assert_ne!(
        create_m, query_m,
        "create and query must have distinct m# tokens"
    );
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

/// Invoke args must reject scalar strings for array-typed params (no comma-split coercion).
#[test]
fn matrix_create_rejects_scalar_for_array_tags_param() {
    let session = langitem_create_query_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "tags");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_title}="hello", {p_tags}="alpha,beta")
created"#,
        item_e = item_e,
        create_m = create_m,
        p_title = p_title,
        p_tags = p_tags,
    );
    assert_compile_rejects_scalar_array_param(&session, "matrix-create-scalar-tags", &source);
}

#[test]
fn matrix_create_accepts_bracket_array_for_tags_param() {
    let session = langitem_create_query_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_score = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_owner = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "tags");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_title}="hello", {p_score}=1, {p_owner}="bot", {p_tags}=["alpha", "beta"])
created"#,
        item_e = item_e,
        create_m = create_m,
        p_title = p_title,
        p_score = p_score,
        p_owner = p_owner,
        p_tags = p_tags,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-create-bracket-tags",
        &source,
    )
    .expect("bracket array tags must compile");
    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run preflight");
}

/// Multiline heredoc inside a method-call argument list must parse as one statement.
#[test]
fn matrix_inline_heredoc_in_create_invoke_compiles() {
    let session = langitem_create_query_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_score = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_owner = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_title}=<<PLASM_MATRIX_INLINE
line one
line two
PLASM_MATRIX_INLINE,
  {p_score}=1,
  {p_owner}="bot")
created"#,
        item_e = item_e,
        create_m = create_m,
        p_title = p_title,
        p_score = p_score,
        p_owner = p_owner,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-inline-heredoc-create",
        &source,
    )
    .expect("inline heredoc in invoke must compile");
    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run preflight");
}

fn compound_branch_mutator_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("load plasm_language_matrix"),
    );
    let endpoints = ["LangItem", "CompoundBranch", "LangTag"]
        .iter()
        .map(|e| plasm_core::ExposureEntityKey {
            entry_id: "langmatrix".into(),
            entity: plasm_core::EntityName::from(*e),
        })
        .collect::<Vec<_>>();
    let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
        cgs.as_ref(),
        "langmatrix",
        "patch lang item tags and resolve compound branch identity",
        &endpoints,
        &[
            "LangItem".to_string(),
            "CompoundBranch".to_string(),
            "LangTag".to_string(),
        ],
        Some(&[
            "langitem_create".to_string(),
            "langitem_update".to_string(),
            "langtag_query".to_string(),
            "langcompoundbranch_get".to_string(),
        ]),
        plasm_core::discovery::ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    let exp = TeachingExposureSession::new_with_intent_delta(
        cgs.as_ref(),
        "langmatrix",
        &["LangItem", "CompoundBranch", "LangTag"],
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
        vec!["LangItem".into(), "CompoundBranch".into(), "LangTag".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

/// Compound GET keys must resolve against CompoundBranch `key_vars`, not ambient homograph slots.
#[test]
fn matrix_compound_get_accepts_opaque_key_symbols_with_mutators_exposed() {
    let session = compound_branch_mutator_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let branch_e = map.entity_sym_for("langmatrix", "CompoundBranch");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_create_title =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_create_score =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_create_owner =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_branch_owner = map.ident_sym_entity_field_for("langmatrix", "CompoundBranch", "owner");
    let p_branch_item_id =
        map.ident_sym_entity_field_for("langmatrix", "CompoundBranch", "item_id");
    let p_branch_name = map.ident_sym_entity_field_for("langmatrix", "CompoundBranch", "name");
    let p_item_owner = map.ident_sym_entity_field_for("langmatrix", "LangItem", "owner");
    let p_item_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
    let p_tag_item_id = map.ident_sym_entity_field_for("langmatrix", "LangTag", "item_id");
    assert_eq!(
        p_branch_item_id, p_tag_item_id,
        "matrix fixture shares item_id value_ref across LangTag + CompoundBranch"
    );
    let source = format!(
        r#"created = {item_e}.{create_m}({p_create_title}="matrix item", {p_create_score}=1, {p_create_owner}="bot")
branch = {branch_e}({p_branch_owner}=created.{p_item_owner}, {p_branch_item_id}=created.{p_item_id}, {p_branch_name}="main")
branch"#,
        item_e = item_e,
        create_m = create_m,
        p_create_title = p_create_title,
        p_create_score = p_create_score,
        p_create_owner = p_create_owner,
        branch_e = branch_e,
        p_branch_owner = p_branch_owner,
        p_branch_item_id = p_branch_item_id,
        p_branch_name = p_branch_name,
        p_item_owner = p_item_owner,
        p_item_id = p_item_id,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-compound-get-homograph-keys",
        &source,
    )
    .expect("compound GET with homograph p# keys must compile");
    let branch = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "branch")
        .expect("branch node");
    assert_eq!(branch["kind"], "get");
    evaluate_plasm_plan_dry(&session, &plan).expect("compound homograph dry-run");
}

/// Plural tag rows projected to a column must lower to an array invoke arg at dry + live staging.
#[test]
fn matrix_update_accepts_column_projection_array_from_plural_tags() {
    let session = compound_branch_mutator_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let tag_e = map.entity_sym_for("langmatrix", "LangTag");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let update_m = map.method_sym_for("langmatrix", "LangItem", "langitem_update");
    let p_create_title =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_create_score =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_create_owner =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_tag_item =
        map.ident_sym_cap_param_for("langmatrix", "LangTag", "langtag_query", "item_id");
    let p_tag_label = map.ident_sym_entity_field_for("langmatrix", "LangTag", "label");
    let p_update_tags =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_update", "tags");
    let p_item_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_create_title}="matrix item", {p_create_score}=1, {p_create_owner}="bot")
tags = {tag_e}{{{p_tag_item}=created.{p_item_id}}}[{p_tag_label}]
updated = {item_e}({p_item_id}=created.{p_item_id}).{update_m}({p_update_tags}=tags.{p_tag_label})
updated"#,
        item_e = item_e,
        create_m = create_m,
        p_create_title = p_create_title,
        p_create_score = p_create_score,
        p_create_owner = p_create_owner,
        tag_e = tag_e,
        p_tag_item = p_tag_item,
        p_item_id = p_item_id,
        p_tag_label = p_tag_label,
        update_m = update_m,
        p_update_tags = p_update_tags,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-update-column-tags-array",
        &source,
    )
    .expect("column projection tags invoke must compile");
    let updated = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "updated")
        .expect("updated node");
    let ir_blob = updated
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| updated.get("ir_template"))
        .expect("updated plan IR");
    assert!(
        ir_blob.to_string().contains("langitem_update"),
        "updated node must dispatch langitem_update: {ir_blob}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("column tags array dry staging");
}

fn langitem_query_update_tags_session() -> ExecuteSession {
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
        "query and patch lang item tags",
        &endpoints,
        &["LangItem".to_string()],
        Some(&[
            "langitem_create".to_string(),
            "langitem_query".to_string(),
            "langitem_update".to_string(),
        ]),
        plasm_core::discovery::ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
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

/// Site-scoped homograph rejections: query `p#` on invoke and mutator `p#` in query filters.
#[test]
fn matrix_homograph_rejects_cross_role_p_sym_bindings() {
    let session = langitem_query_update_tags_session();
    let map = github_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let update_m = map.method_sym_for("langmatrix", "LangItem", "langitem_update");
    let p_create_title =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_create_score =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_create_owner =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_query_team =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_query", "team_key");
    let p_update_tags =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_update", "tags");
    let p_item_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");

    if !SymbolMap::is_opaque_p_sym(p_query_team.as_str())
        || !SymbolMap::is_opaque_p_sym(p_update_tags.as_str())
    {
        return;
    }

    let invoke_reject_source = format!(
        r#"created = {item_e}.{create_m}({p_create_title}="matrix item", {p_create_score}=1, {p_create_owner}="bot")
updated = {item_e}({p_item_id}=created.{p_item_id}).{update_m}({p_query_team}=["alpha"])
updated"#,
        item_e = item_e,
        create_m = create_m,
        update_m = update_m,
        p_create_title = p_create_title,
        p_create_score = p_create_score,
        p_create_owner = p_create_owner,
        p_query_team = p_query_team,
        p_item_id = p_item_id,
    );
    let query_reject_source = format!(
        r#"items = {item_e}{{{p_update_tags}=["alpha"]}}"#,
        item_e = item_e,
        p_update_tags = p_update_tags,
    );

    for (name, source) in [
        ("matrix-update-query-p-reject", invoke_reject_source),
        ("matrix-query-update-p-reject", query_reject_source),
    ] {
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            name,
            &source,
        )
        .expect_err(name);
        let msg = err.to_string();
        if name.contains("update-query") {
            assert_compile_rejects_unknown_cap_param(&msg);
        } else {
            assert_compile_rejects_query_filter_psym(&msg);
        }
    }
}
