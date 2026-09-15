//! Language-matrix symbol-resolution regressions (projection + cap-qualified mutator args).

use super::test_support::{
    assert_compile_rejects_scalar_array_param, assert_compile_rejects_unknown_cap_param,
    compile_matrix_program, langitem_ranked_mutator_session, langitem_tag_session, matrix_cgs,
    matrix_symbol_map,
};
use crate::plasm_dag::compile_plasm_dag_to_plan;
use crate::plasm_dag::ExecuteSession;
use crate::plasm_plan_run::evaluate_plasm_plan_dry;
use plasm_core::TeachingExposureSession;
use std::sync::Arc;

/// Unknown field wire on LangTag projection surfaces an honest error (no LangItem `title` phantom).
#[test]
fn label_projection_unknown_p_sym_is_typed_error_not_phantom_body() {
    let session = langitem_tag_session();
    let map = matrix_symbol_map(&session);
    let label_e = map.entity_sym_for("langmatrix", "LangTag");
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let item_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
    let source = format!(
        r#"item = {item_e}({item_id}="i1")
labels = {label_e}{{{p_item}=item.{item_id}}}
labels | select body"#,
        item_e = item_e,
        item_id = item_id,
        label_e = label_e,
        p_item = map.ident_sym_cap_param_for("langmatrix", "LangTag", "langtag_query", "item_id"),
    );
    let err = compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &session,
        "tag-bad-body-wire",
        &source,
    )
    .expect_err("LangItem body wire must not project LangTag rows");
    let msg = err.to_string();
    assert!(
        msg.contains("not a row symbol")
            || msg.contains("not a row field")
            || msg.contains("expected entity field"),
        "expected typed symbol error, got {msg}"
    );
    assert!(
        msg.contains("LangTag") && msg.contains("body") && msg.contains("not a row field"),
        "must reject LangItem body homograph on LangTag projection: {msg}"
    );
}

/// LangTag query projection must resolve tokens against the LangTag row contract, not
/// globally homographed LangItem field wires (`title`, `status`).
#[test]
fn label_query_projection_resolves_entity_scoped_p_symbols() {
    let session = langitem_tag_session();
    let map = matrix_symbol_map(&session);
    let p_label = map.ident_sym_entity_field_for("langmatrix", "LangTag", "label");
    let p_seq = map.ident_sym_entity_field_for("langmatrix", "LangTag", "seq");
    let p_item_id = map.ident_sym_entity_field_for("langmatrix", "LangTag", "item_id");
    let label_e = map.entity_sym_for("langmatrix", "LangTag");
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let item_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
    let p_item = map.ident_sym_cap_param_for("langmatrix", "LangTag", "langtag_query", "item_id");
    let source = format!(
        r#"item = {item_e}({item_id}="i1")
labels = {label_e}{{{p_item}=item.{item_id}}}
labels | select {p_label},{p_seq},{p_item_id}"#,
    );
    let plan = compile_matrix_program(&session, "langtag-projection", &source);
    let return_id = plan["return"]["node"].as_str().expect("return node id");
    let return_node = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == return_id)
        .expect("return node");
    assert_eq!(return_node["kind"], "compute");
    let fields = return_node["compute"]["op"]["fields"]
        .as_object()
        .expect("project fields");
    assert!(
        fields.contains_key("label")
            && fields.contains_key("seq")
            && fields.contains_key("item_id"),
        "expected LangTag wire columns, got {fields:?}"
    );
    assert!(
        !fields.contains_key("title") && !fields.contains_key("status"),
        "must not project LangItem homograph wires onto LangTag rows: {fields:?}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("langtag projection dry-run");
}

/// `langitem_create` dotted-call args must resolve cap-qualified `p#` for homograph params.
#[test]
fn issue_create_dry_run_resolves_cap_qualified_param_symbols() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem"],
        "create a lang item with title and tags",
        &["langitem_create"],
        "langitem_create",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let cap = cgs
        .get_capability("langitem_create")
        .expect("langitem_create");
    let method_sym = map.method_sym_for("langmatrix", "LangItem", cap.name.as_str());
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_owner = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_score = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "tags");
    let source = format!(
        r#"created = {item_e}.{method_sym}({p_title}="Document tags", {p_owner}="alice", {p_score}=1, {p_tags}=["bug", "docs"])
created"#,
    );
    let plan = compile_matrix_program(&session, "langitem-create", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    assert_eq!(created["kind"], "create");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    assert!(
        ir_blob.to_string().contains("langitem_create"),
        "expected langitem_create in plan IR, got {ir_blob}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("langitem_create dry-run");
}

#[test]
fn issue_create_ir_input_uses_logical_param_names_not_opaque_p_symbols() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem"],
        "create a lang item with title and tags",
        &["langitem_create"],
        "langitem_create",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let cap = cgs
        .get_capability("langitem_create")
        .expect("langitem_create");
    let method_sym = map.method_sym_for("langmatrix", "LangItem", cap.name.as_str());
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_owner = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let p_score = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "tags");
    let source = format!(
        r#"created = {item_e}.{method_sym}({p_title}="Document tags", {p_owner}="alice", {p_score}=1, {p_tags}=["bug", "docs"])
created"#,
    );
    let plan = compile_matrix_program(&session, "langitem-create-p-syms", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    let expr_json = ir_blob
        .pointer("/expr")
        .or_else(|| ir_blob.get("expr"))
        .expect("template expr");
    let expr: plasm_core::Expr =
        serde_json::from_value(expr_json.clone()).expect("deserialize create IR");
    let plasm_core::Expr::Create(create) = expr else {
        panic!("expected Create IR, got {expr:?}");
    };
    let plasm_core::Value::Object(input) = create.input.to_value() else {
        panic!("expected object invoke input, got {:?}", create.input);
    };
    for key in ["title", "owner", "tags"] {
        assert!(
            input.contains_key(key),
            "invoke input must use logical param `{key}`, got keys {:?}",
            input.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !input
            .keys()
            .any(|k| plasm_core::symbol_tuning::SymbolMap::is_opaque_p_sym(k)),
        "invoke input must not retain opaque p# keys: {input:?}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("staged langitem_create dry-run preflight");
}

#[test]
fn issue_create_rejects_scalar_for_array_labels_param() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem"],
        "create a lang item with title and tags",
        &["langitem_create"],
        "langitem_create",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let method_sym = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "tags");
    let source = format!(
        r#"created = {item_e}.{method_sym}({p_title}="Document tags", {p_tags}="enhancement,documentation")
created"#,
    );
    assert_compile_rejects_scalar_array_param(&session, "langitem-create-scalar-tags", &source);
}

#[test]
fn issue_update_rejects_scalar_for_array_labels_param() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem"],
        "update lang item tags",
        &["langitem_update"],
        "langitem_update",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let update_m = map.method_sym_for("langmatrix", "LangItem", "langitem_update");
    let p_tags = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_update", "tags");
    let source = format!(r#"{item_e}("i1").{update_m}({p_tags}="enhancement,documentation")"#,);
    assert_compile_rejects_scalar_array_param(&session, "langitem-update-scalar-tags", &source);
}

/// Open with LangItem then expand — wave-1 `m#`/`p#` stay stable and taught create still compiles.
#[test]
fn cross_wave_matrix_incremental_exposure_symbol_stability() {
    use plasm_core::CgsContext;

    let cgs = matrix_cgs();
    let layers: Vec<&plasm_core::CGS> = vec![cgs.as_ref()];

    let w1 = plasm_core::capability_exposure::explicit_entity_capability_surface(
        cgs.as_ref(),
        "langmatrix",
        &["LangItem".to_string()],
    )
    .expect("explicit fixture capability exposure");
    let mut exp = TeachingExposureSession::new_with_intent_delta(
        cgs.as_ref(),
        "langmatrix",
        &["LangItem"],
        w1,
    );
    let m_create_w1 =
        exp.symbol_map_arc()
            .method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title_w1 = exp.symbol_map_arc().ident_sym_cap_param_for(
        "langmatrix",
        "LangItem",
        "langitem_create",
        "title",
    );
    let title_field_p =
        exp.symbol_map_arc()
            .ident_sym_entity_field_for("langmatrix", "LangItem", "title");

    let new_seeds = ["LangTag", "LangLine"];
    let w2 = plasm_core::capability_exposure::explicit_entity_capability_surface(
        cgs.as_ref(),
        "langmatrix",
        &new_seeds.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
    )
    .expect("explicit fixture capability exposure");
    exp.expose_surface(&layers, cgs.clone(), "langmatrix", &new_seeds, w2);

    let map = exp.symbol_map_arc();
    let m_create_w2 = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title_w2 =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    assert_eq!(
        map.ident_sym_entity_field_for("langmatrix", "LangItem", "title"),
        title_field_p,
        "LangItem.title p# must stay stable across expand wave"
    );

    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    let session = ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into(), "LangTag".into(), "LangLine".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );

    let smap = matrix_symbol_map(&session);
    let item_e = smap.entity_sym_for("langmatrix", "LangItem");
    let create_m = smap.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let p_title =
        smap.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let taught = format!(
        r#"created = {item_e}.{create_m}({p_title}="Document tags")
created"#,
    );
    let taught_res = compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &session,
        "langitem-incremental-taught",
        &taught,
    );

    assert_eq!(
        m_create_w1, m_create_w2,
        "langitem_create m# must be stable across waves"
    );
    assert_eq!(
        p_title_w1, p_title_w2,
        "langitem_create title p# must be stable across waves"
    );
    assert!(
        taught_res.is_ok(),
        "taught create form must compile after incremental expansion: {taught_res:?}"
    );
}

/// Session `m#` must resolve to the catalog capability from `sym_to_method`.
#[test]
fn issue_create_opaque_m_resolves_from_session_map() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem"],
        "create a lang item and ping it",
        &["langitem_create", "langitem_ping"],
        "langitem_create",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let create_m = map.method_sym_for("langmatrix", "LangItem", "langitem_create");
    assert!(
        create_m.starts_with('m'),
        "langitem_create must have session m#: {create_m}"
    );
    let p_title = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
    let p_score = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "score");
    let p_owner = map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
    let source = format!(
        r#"created = {item_e}.{create_m}({p_title}="Label guide", {p_score}=1, {p_owner}="alice")
created"#,
    );
    let plan = compile_matrix_program(&session, "langitem-create-session-m", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    assert_eq!(created["kind"], "create");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    assert!(
        ir_blob.to_string().contains("langitem_create"),
        "session m# must bind langitem_create, not ping: {ir_blob}"
    );
    assert!(
        !ir_blob.to_string().contains("langitem_ping"),
        "must not resolve kebab homograph: {ir_blob}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("langitem_create session m# dry-run");
}

/// `langitem_update.tags` must resolve only via cap-qualified invoke context, not LangTag row-field homographs.
#[test]
fn issue_update_invoke_rejects_unqualified_label_name_homograph_p() {
    let cgs = matrix_cgs();
    let session = langitem_ranked_mutator_session(
        &cgs,
        &["LangItem", "LangTag"],
        "update lang item tags",
        &["langitem_update"],
        "langitem_update",
    );
    let map = matrix_symbol_map(&session);
    let item_e = map.entity_sym_for("langmatrix", "LangItem");
    let update_m = map.method_sym_for("langmatrix", "LangItem", "langitem_update");
    let p_tag_label = map.ident_sym_entity_field_for("langmatrix", "LangTag", "label");
    let p_update_tags =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_update", "tags");
    if !plasm_core::symbol_tuning::SymbolMap::is_opaque_p_sym(p_tag_label.as_str()) {
        return;
    }
    let bad_source = format!(r#"{item_e}("i1").{update_m}({p_tag_label}=["bug"])"#,);
    let err = compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &session,
        "langitem-update-tag-label-homograph",
        &bad_source,
    )
    .expect_err("LangTag.label p# must not bind langitem_update.tags");
    assert_compile_rejects_unknown_cap_param(&err.to_string());
    let good_source = format!(r#"{item_e}("i1").{update_m}({p_update_tags}=["bug"])"#,);
    let plan = compile_matrix_program(&session, "langitem-update-cap-qualified-tags", &good_source);
    evaluate_plasm_plan_dry(&session, &plan).expect("cap-qualified tags invoke dry-run");
}
