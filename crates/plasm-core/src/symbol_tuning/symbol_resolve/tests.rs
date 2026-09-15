use super::*;
use crate::load_schema;
use crate::loader::load_schema_dir;
use crate::symbol_tuning::TeachingExposureSession;

use std::path::PathBuf;

#[test]
fn lookup_langitem_create_in_federated_layers() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs_a = load_schema(&dir).expect("plasm_language_matrix");
    cgs_a.bind_registry_entry_id("langmatrix_a");
    let mut cgs_b = load_schema(&dir).expect("plasm_language_matrix");
    cgs_b.bind_registry_entry_id("langmatrix_b");
    let layers = [
        CgsLayer::new("langmatrix_a", &cgs_a),
        CgsLayer::new("langmatrix_b", &cgs_b),
    ];
    let cap =
        lookup_capability_in_layer_stack(&layers, "langmatrix_b", "LangItem", "langitem_create")
            .expect("langmatrix_b langitem_create");
    assert_eq!(cap.name.as_str(), "langitem_create");
    assert_eq!(cap.domain.as_str(), "LangItem");
}

#[test]
fn resolve_entity_field_unknown_opaque_p_sym() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/capability_with_input");
    let cgs = load_schema_dir(&dir).expect("capability_with_input");
    let exp = TeachingExposureSession::new(&cgs, "", &["Account"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("Account").expect("Account");
    let err = map
        .resolve_entity_field(CatalogScope::SessionReverse, "Account", ent, "p999")
        .expect_err("unknown p#");
    assert!(matches!(err, SymbolResolveError::UnknownSessionPSym { .. }));
}

#[test]
fn resolve_cap_param_accepts_session_reverse_opaque_p_on_unset_fixture() {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    if !dir.is_dir() {
        return;
    }
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new(&cgs, "", &["PromptRun"]);
    let map = exp.symbol_map_arc();
    let cap = cgs.capabilities.get("prompt_run_create").expect("cap");
    let slug = map.ident_sym_cap_param_for("", "PromptRun", "prompt_run_create", "slug");
    let wire = map
        .resolve_cap_param(
            CatalogScope::SessionReverse,
            "PromptRun",
            "prompt_run_create",
            slug.as_str(),
            cap,
        )
        .expect("slug wire on unset single-graph fixture");
    assert_eq!(wire, "slug");
}

#[test]
fn resolve_entity_field_rejects_cross_entity_homograph() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
    let map = exp.symbol_map_arc();
    let row_a_wire = "headline";
    let ent_b = cgs.get_entity("HomographRowB").expect("HomographRowB");
    let err = map
        .resolve_entity_field(
            CatalogScope::qualified("langmatrix"),
            "HomographRowB",
            ent_b,
            row_a_wire,
        )
        .expect_err("HomographRowA wire must not resolve on HomographRowB");
    assert!(matches!(
        err,
        SymbolResolveError::UnknownEntityPSym { .. } | SymbolResolveError::NotARowField { .. }
    ));
}

#[test]
fn resolve_cap_param_rejects_query_scope_p_on_mutator_invoke() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "langmatrix",
        &["LangItem"],
        crate::capability_exposure::explicit_entity_capability_surface(
            &cgs,
            "langmatrix",
            &["LangItem".to_string()],
        )
        .expect("explicit fixture capability exposure"),
    );
    let map = exp.symbol_map_arc();
    let update_cap = cgs
        .get_capability("langitem_update")
        .expect("langitem_update");
    let err = map
        .resolve_cap_param(
            CatalogScope::qualified("langmatrix"),
            "LangItem",
            "langitem_update",
            "team_key",
            update_cap,
        )
        .expect_err("query team_key wire must not resolve on update invoke");
    assert!(err.is_unknown_cap_param());
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("langmatrix"),
            "LangItem",
            "langitem_update",
            "tags",
            update_cap,
        )
        .expect("update tags wire on update invoke");
    assert_eq!(wire, "tags");
}

#[test]
fn resolve_query_filter_field_accepts_cap_scope_param_p_sym() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    cgs.bind_registry_entry_id("langmatrix");
    let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["LangItem", "LangTag"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("LangTag").expect("LangTag");
    let wire = map
        .resolve_query_filter_field(
            CatalogScope::qualified("langmatrix"),
            "LangTag",
            ent,
            &cgs,
            "item_id",
        )
        .expect("langtag_query item_id scope param");
    assert_eq!(wire, "item_id");
}

#[test]
fn resolve_entity_field_projection_stable_after_exposure_extend() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let mut exp = TeachingExposureSession::new(&cgs, entry, &["LangItem"]);
    let map0 = exp.symbol_map_arc();
    let ent = cgs.get_entity("LangItem").expect("LangItem");
    let title_wire = map0
        .resolve_entity_field(CatalogScope::qualified(entry), "LangItem", ent, "title")
        .expect("wave-1 title projection");
    assert_eq!(title_wire, "title");

    exp.expose_entities(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        entry,
        &["LangTag"],
    );
    let map1 = exp.symbol_map_arc();
    let title_wire_after = map1
        .resolve_entity_field(CatalogScope::qualified(entry), "LangItem", ent, "title")
        .expect("wave-2 title projection");
    assert_eq!(
        title_wire_after, title_wire,
        "extend wave must not remap wave-1 wire projection"
    );
}

/// Federated extend: colliding `name` wire on WaveLang vs WaveMon resolves by receiver entity.
#[test]
fn resolve_entity_field_federated_wire_name_by_receiver_after_extend() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/extend_wave_homograph");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "wavehom";
    let mut exp = TeachingExposureSession::new(&cgs, entry, &["WaveLang"]);
    let map0 = exp.symbol_map_arc();
    let lang_ent = cgs.get_entity("WaveLang").expect("WaveLang");
    assert_eq!(
        map0.resolve_entity_field(CatalogScope::qualified(entry), "WaveLang", lang_ent, "name",)
            .expect("wave-1"),
        "name"
    );

    exp.expose_entities(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        entry,
        &["WaveMon"],
    );
    let map1 = exp.symbol_map_arc();
    let mon_ent = cgs.get_entity("WaveMon").expect("WaveMon");
    assert_eq!(
        map1.resolve_entity_field(CatalogScope::qualified(entry), "WaveLang", lang_ent, "name",)
            .expect("lang name after extend"),
        "name"
    );
    assert_eq!(
        map1.resolve_entity_field(CatalogScope::qualified(entry), "WaveMon", mon_ent, "name",)
            .expect("mon name after extend"),
        "name"
    );
    assert_eq!(
        map1.resolve_entity_field(
            CatalogScope::qualified(entry),
            "WaveLang",
            lang_ent,
            "official",
        )
        .expect("lang official"),
        "official"
    );
}

#[test]
fn resolve_cap_param_shared_scope_p_on_create_when_only_query_committed() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    cgs.bind_registry_entry_id("langmatrix");
    let exp = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "langmatrix",
        &["LangItem"],
        crate::capability_exposure::explicit_entity_capability_surface(
            &cgs,
            "langmatrix",
            &["LangItem".to_string()],
        )
        .expect("explicit fixture capability exposure"),
    );
    let map = exp.symbol_map_arc();
    let create_cap = cgs
        .get_capability("langitem_create")
        .expect("langitem_create");
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("langmatrix"),
            "LangItem",
            "langitem_create",
            "tags",
            create_cap,
        )
        .expect("shared tags wire must resolve on langitem_create invoke");
    assert_eq!(wire, "tags");
}

#[test]
fn agent_program_error_includes_query_filter_hint() {
    let err = SymbolResolveError::UnknownQueryFilterPSym {
        entity: "Label".into(),
        token: "p99".into(),
        method_invoke_form: None,
    };
    let msg = err.to_agent_program_error();
    assert!(msg.contains("query filter symbol"));
    assert!(msg.contains("help:"));
    assert!(msg.contains("query/search filters"));
}

#[test]
fn mutator_param_misused_as_row_or_filter_hints_invoke_form() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["LangAuthSession"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("LangAuthSession").expect("LangAuthSession");
    // `password` is login payload only — not a row field or query filter.
    let row_err = map
        .resolve_entity_field(
            CatalogScope::qualified(entry),
            "LangAuthSession",
            ent,
            "password",
        )
        .expect_err("password must not resolve as row field");
    let row_msg = row_err.to_string();
    assert!(
        row_msg.contains("method parameter") && row_msg.contains("password="),
        "expected mutator-param redirect on row field, got: {row_msg}"
    );
    let filter_err = map
        .resolve_query_filter_field(
            CatalogScope::qualified(entry),
            "LangAuthSession",
            ent,
            &cgs,
            "password",
        )
        .expect_err("password must not resolve as query filter");
    let filter_msg = filter_err.to_string();
    assert!(
        filter_msg.contains("method parameter") && filter_msg.contains("password="),
        "expected mutator-param redirect on filter, got: {filter_msg}"
    );
    assert!(
        filter_err
            .to_agent_program_error()
            .contains("method/create parameter"),
        "expected agent hint toward method form"
    );
}

#[test]
fn query_selection_param_misused_as_row_field_hints_brace_not_method() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["LangItem"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("LangItem").expect("LangItem");
    let row_err = map
        .resolve_entity_field(CatalogScope::qualified(entry), "LangItem", ent, "q")
        .expect_err("q is search selection, not a row field");
    let row_msg = row_err.to_string();
    assert!(
        row_msg.contains("query selection") && row_msg.contains("{q=...}"),
        "expected query-brace redirect, got: {row_msg}"
    );
    assert!(
        !row_msg.contains(".m") && !row_msg.contains("<method>"),
        "query selection must not be advertised as e#.m#: {row_msg}"
    );
    assert!(
        row_err
            .to_agent_program_error()
            .contains("query/search selection slot"),
        "expected agent hint toward query braces"
    );
}

#[test]
fn resolve_cap_param_homographed_create_update_title() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    cgs.bind_registry_entry_id("langmatrix");
    let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["LangItem"]);
    let map = exp.symbol_map_arc();
    let create = cgs
        .capabilities
        .get("langitem_create")
        .expect("langitem_create");
    let update = cgs
        .capabilities
        .get("langitem_update")
        .expect("langitem_update");
    let w1 = map
        .resolve_cap_param(
            CatalogScope::qualified("langmatrix"),
            "LangItem",
            "langitem_create",
            "title",
            create,
        )
        .expect("create title");
    let w2 = map
        .resolve_cap_param(
            CatalogScope::qualified("langmatrix"),
            "LangItem",
            "langitem_update",
            "title",
            update,
        )
        .expect("update title");
    assert_eq!(w1, "title");
    assert_eq!(w2, "title");
}

#[test]
fn opaque_query_m_sym_rejected_on_mutator_payload_dotted_call() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["LangItem"]);
    let map = exp.symbol_map_arc();
    let e_sym = map.entity_sym_for(entry, "LangItem");
    let m_sym = map.method_sym_for(entry, "LangItem", "langitem_query");
    let tags_wire = "tags";
    let line = format!("{e_sym}($).{m_sym}({tags_wire}=$)");
    let err =
        crate::expr_parser::parse_session_line(
            &line,
            &cgs,
            Some(std::sync::Arc::clone(&map)
                as std::sync::Arc<dyn crate::symbol_tuning::SymbolSession>),
        )
        .expect_err("query m# must not bind as mutator payload");
    let msg = err.message();
    assert!(
        msg.contains("query") && msg.contains(&format!("{e_sym}{{…}}")) && !msg.contains("mutator"),
        "unexpected message: {msg}"
    );
}

#[test]
fn opaque_get_m_sym_names_taught_get_seat_not_mutator() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["LangItem"]);
    let map = exp.symbol_map_arc();
    let e_sym = map.entity_sym_for(entry, "LangItem");
    let m_sym = map.method_sym_for(entry, "LangItem", "langitem_get");
    let line = format!("{e_sym}($).{m_sym}(title=$)");
    let err =
        crate::expr_parser::parse_session_line(
            &line,
            &cgs,
            Some(std::sync::Arc::clone(&map)
                as std::sync::Arc<dyn crate::symbol_tuning::SymbolSession>),
        )
        .expect_err("get m# must not bind as mutator payload");
    let msg = err.message();
    assert!(
        msg.contains("get") && msg.contains(&format!("{e_sym}(<id>)")) && !msg.contains("mutator"),
        "unexpected message: {msg}"
    );
}

#[test]
fn pathless_identity_mutator_names_taught_seat_not_illegal_form() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["LangItem"]);
    let map = exp.symbol_map_arc();
    let e_sym = map.entity_sym_for(entry, "LangItem");
    let m_sym = map.method_sym_for(entry, "LangItem", "langitem_update");
    let illegal = format!(r#"{e_sym}.{m_sym}(title="x")"#);
    let err =
        crate::expr_parser::parse_session_line(
            &illegal,
            &cgs,
            Some(std::sync::Arc::clone(&map)
                as std::sync::Arc<dyn crate::symbol_tuning::SymbolSession>),
        )
        .expect_err("pathless identity mutator must still fail");
    let msg = err.message();
    let taught = format!("{e_sym}(<id>).{m_sym}");
    assert!(
        msg.contains(&taught),
        "diagnostic must name taught seat `{taught}`, got: {msg}"
    );
    assert!(
        !msg.contains(&format!("requires `{e_sym}.{m_sym}")),
        "must not advertise pathless `{e_sym}.{m_sym}` as required, got: {msg}"
    );
    assert!(
        !msg.contains(&format!("Required by {e_sym}.{m_sym}")),
        "must not require pathless basename, got: {msg}"
    );

    let lawful = format!(r#"{e_sym}("i1").{m_sym}(title="x")"#);
    crate::expr_parser::parse_session_line(
        &lawful,
        &cgs,
        Some(std::sync::Arc::clone(&map) as std::sync::Arc<dyn crate::symbol_tuning::SymbolSession>),
    )
    .expect("taught identity seat must still parse");
}

#[test]
fn compound_key_accepts_wire_names() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let entry = "langmatrix";
    let exp = TeachingExposureSession::new(&cgs, entry, &["CompoundBranch", "LangItem"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("CompoundBranch").expect("CompoundBranch");
    for sym in ["owner", "item_id", "name"] {
        let wire = map
            .resolve_compound_key(
                CatalogScope::qualified(entry),
                "CompoundBranch",
                &ent.key_vars,
                sym,
            )
            .unwrap_or_else(|e| panic!("compound key {sym}: {e:?}"));
        assert_eq!(wire, sym);
    }
    let e_sym = map.entity_sym_for(entry, "CompoundBranch");
    let get_line = format!("{e_sym}(owner=acme, item_id=i1, name=main)");
    let mut parsed =
        crate::expr_parser::parse_session_line(
            &get_line,
            &cgs,
            Some(std::sync::Arc::clone(&map)
                as std::sync::Arc<dyn crate::symbol_tuning::SymbolSession>),
        )
        .expect("compound get with wire keys");
    crate::normalize_expr_query_capabilities(&mut parsed.expr, &cgs).expect("normalize");
    crate::type_check_expr(&parsed.expr, &cgs).expect("typecheck get");
    let crate::Expr::Get(g) = &parsed.expr else {
        panic!("expected Get");
    };
    let crate::EntityKey::Compound(m) = &g.reference.key else {
        panic!("expected compound key");
    };
    assert_eq!(m.get("owner").and_then(|s| s.as_lit_str()), Some("acme"));
    assert_eq!(m.get("item_id").and_then(|s| s.as_lit_str()), Some("i1"));
    assert_eq!(m.get("name").and_then(|s| s.as_lit_str()), Some("main"));
}

/// Regression: deleted qualified reverse-map fields must not reappear on opaque resolution paths.
#[test]
fn opaque_resolution_source_has_no_qualified_reverse_maps() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        manifest.join("src/symbol_tuning/mod.rs"),
        manifest.join("src/expr_parser/mod.rs"),
        manifest.join("src/relation_segment.rs"),
    ];
    let forbidden = [
        "entity_p_sym_to_wire",
        "cap_p_sym_to_param",
        "entity_p_sym_globally_unique",
        "rebuild_qualified_p_sym_indexes",
        "resolve_wire_for_p_sym",
    ];
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("read source");
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{} must not reference deleted reverse-map `{}`",
                path.display(),
                needle
            );
        }
    }
}
