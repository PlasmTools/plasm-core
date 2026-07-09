use super::*;
use crate::load_schema;
use crate::loader::load_schema_dir;
use crate::symbol_tuning::TeachingExposureSession;
use std::path::PathBuf;

#[test]
fn lookup_linear_issue_create_in_federated_layers() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pokeapi_dir = root.join("../../apis/pokeapi");
    let linear_dir = root.join("../../apis/linear");
    if !pokeapi_dir.is_dir() || !linear_dir.is_dir() {
        return;
    }
    let cgs_pokeapi = load_schema(&pokeapi_dir).expect("pokeapi");
    let cgs_linear = load_schema(&linear_dir).expect("linear");
    let layers = [
        CgsLayer::new("pokeapi", &cgs_pokeapi),
        CgsLayer::new("linear", &cgs_linear),
    ];
    let cap = lookup_capability_in_layer_stack(&layers, "linear", "Issue", "issue_create")
        .expect("linear issue_create");
    assert_eq!(cap.name.as_str(), "issue_create");
    assert_eq!(cap.domain.as_str(), "Issue");
}

#[test]
fn resolve_entity_field_unknown_opaque_p_sym() {
    // Use a teaching-complete fixture (`overshow_tools`/`PromptRun`, proven by
    // `overshow_tools_teaching_bundle_covers_all_capabilities`). The p# resolution error under
    // test is orthogonal to the fixture; the previously-used `capability_with_input` has an
    // update-only `Account` whose session exposure yields an empty teaching block.
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
    let ent = cgs.get_entity("PromptRun").expect("PromptRun");
    let err = map
        .resolve_entity_field(CatalogScope::SessionReverse, "PromptRun", ent, "p999")
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
    use crate::discovery;
    use crate::EntityName;
    use crate::ExposureEntityKey;

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "langmatrix",
        &["LangItem"],
        discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "langmatrix",
            "query and patch lang item tags",
            &[ExposureEntityKey {
                entry_id: "langmatrix".into(),
                entity: EntityName::from("LangItem"),
            }],
            &["LangItem".to_string()],
            Some(&["langitem_query".to_string(), "langitem_update".to_string()]),
            discovery::ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        ),
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
    std::env::set_var("PLASM_CGS_FAST_LOAD", "1");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.is_dir() {
        return;
    }
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue", "Label"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("Label").expect("Label");
    let wire = map
        .resolve_query_filter_field(
            CatalogScope::qualified("github"),
            "Label",
            ent,
            &cgs,
            "repository",
        )
        .expect("label_query repository scope param");
    assert_eq!(wire, "repository");
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
fn resolve_cap_param_shared_scope_p_on_issue_create_when_only_issue_query_committed() {
    use crate::discovery;
    use crate::EntityName;
    use crate::ExposureEntityKey;

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.is_dir() {
        return;
    }
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "github",
        &["Issue"],
        discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "github",
            "query issues in repository",
            &[ExposureEntityKey {
                entry_id: "github".into(),
                entity: EntityName::from("Issue"),
            }],
            &["Issue".to_string()],
            Some(&["issue_query".to_string()]),
            discovery::ExposureSurfaceOptions {
                read_first_seeded: false,
            },
        ),
    );
    let map = exp.symbol_map_arc();
    let create_cap = cgs.get_capability("issue_create").expect("issue_create");
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("github"),
            "Issue",
            "issue_create",
            "repository",
            create_cap,
        )
        .expect("shared repository wire must resolve on issue_create invoke");
    assert_eq!(wire, "repository");
}

#[test]
fn agent_program_error_includes_query_filter_hint() {
    let err = SymbolResolveError::UnknownQueryFilterPSym {
        entity: "Label".into(),
        token: "p99".into(),
    };
    let msg = err.to_agent_program_error();
    assert!(msg.contains("query filter symbol"));
    assert!(msg.contains("help:"));
    assert!(msg.contains("query/search filters"));
}

#[test]
fn resolve_cap_param_homographed_union_variant_ref_paths() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
    if !dir.is_dir() {
        return;
    }
    let Ok(cgs) = load_schema_dir(&dir) else {
        return;
    };
    let exp = TeachingExposureSession::new(&cgs, "proof", &["Document"]);
    let map = exp.symbol_map_arc();
    let cap = cgs
        .capabilities
        .get("document_edit_v2")
        .expect("document_edit_v2");
    let replace_wire = "operations.replace_block.ref";
    let insert_wire = "operations.insert_before.ref";
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("proof"),
            "Document",
            "document_edit_v2",
            replace_wire,
            cap,
        )
        .expect("union-variant ref wire must resolve on document_edit_v2 invoke");
    assert_eq!(wire, replace_wire);
    let wire2 = map
        .resolve_cap_param(
            CatalogScope::qualified("proof"),
            "Document",
            "document_edit_v2",
            insert_wire,
            cap,
        )
        .expect("second ref wire");
    assert_eq!(wire2, insert_wire);
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
        msg.contains("not a mutator") && msg.contains("query"),
        "unexpected message: {msg}"
    );
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
    assert_eq!(m.get("owner").map(String::as_str), Some("acme"));
    assert_eq!(m.get("item_id").map(String::as_str), Some("i1"));
    assert_eq!(m.get("name").map(String::as_str), Some("main"));
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
