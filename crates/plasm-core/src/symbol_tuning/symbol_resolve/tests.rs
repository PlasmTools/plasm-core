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
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/capability_with_input");
    let cgs = load_schema_dir(&dir).expect("capability_with_input");
    let exp = TeachingExposureSession::new(&cgs, "", &["Account"]);
    let map = exp.symbol_map_arc();
    let ent = cgs.get_entity("Account").expect("Account");
    let err = map
        .resolve_entity_field(CatalogScope::SessionReverse, "Account", ent, "p999")
        .expect_err("unknown p#");
    assert!(matches!(
        err,
        SymbolResolveError::UnknownEntityPSym { .. }
            | SymbolResolveError::UnknownSessionPSym { .. }
    ));
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
        .expect("slug p# on unset single-graph fixture");
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
    let row_a = map.ident_sym_entity_field_for("", "HomographRowA", "headline");
    let ent_b = cgs.get_entity("HomographRowB").expect("HomographRowB");
    let err = map
        .resolve_entity_field(
            CatalogScope::qualified("langmatrix"),
            "HomographRowB",
            ent_b,
            row_a.as_str(),
        )
        .expect_err("HomographRowA p# must not resolve on HomographRowB");
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
    let p_query_team =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_query", "team_key");
    if !SymbolMap::is_opaque_p_sym(p_query_team.as_str()) {
        return;
    }
    let err = map
        .resolve_opaque_p(
            CatalogScope::qualified("langmatrix"),
            PSymResolution::InvokeParam {
                domain: "LangItem",
                capability: "langitem_update",
                cap: update_cap,
            },
            p_query_team.as_str(),
        )
        .expect_err("query team_key p# must not resolve on update invoke");
    assert!(err.is_unknown_cap_param());
    let p_update_tags =
        map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_update", "tags");
    if !SymbolMap::is_opaque_p_sym(p_update_tags.as_str()) {
        return;
    }
    let ent = cgs.get_entity("LangItem").expect("LangItem");
    let err = map
        .resolve_opaque_p(
            CatalogScope::qualified("langmatrix"),
            PSymResolution::QueryFilter {
                entity: "LangItem",
                ent,
                cgs: &cgs,
            },
            p_update_tags.as_str(),
        )
        .expect_err("update tags p# must not resolve as query filter");
    assert!(err.is_unknown_query_filter());
    let wire = map
        .resolve_opaque_p(
            CatalogScope::qualified("langmatrix"),
            PSymResolution::InvokeParam {
                domain: "LangItem",
                capability: "langitem_update",
                cap: update_cap,
            },
            p_update_tags.as_str(),
        )
        .expect("update tags p# on update invoke");
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
    let p_repository = map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
    if !SymbolMap::is_opaque_p_sym(p_repository.as_str()) {
        return;
    }
    let wire = map
        .resolve_query_filter_field(
            CatalogScope::qualified("github"),
            "Label",
            ent,
            &cgs,
            p_repository.as_str(),
        )
        .expect("label_query repository scope param");
    assert_eq!(wire, "repository");
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
    let p_repository = map.ident_sym_cap_param_for("github", "Issue", "issue_query", "repository");
    if !SymbolMap::is_opaque_p_sym(p_repository.as_str()) {
        return;
    }
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("github"),
            "Issue",
            "issue_create",
            p_repository.as_str(),
            create_cap,
        )
        .expect("shared repository p# must resolve on issue_create invoke");
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
    assert!(msg.contains("query/search input signature"));
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
    let replace_ref = map.ident_sym_cap_param_for(
        "proof",
        "Document",
        "document_edit_v2",
        "operations.replace_block.ref",
    );
    let insert_ref = map.ident_sym_cap_param_for(
        "proof",
        "Document",
        "document_edit_v2",
        "operations.insert_before.ref",
    );
    assert_eq!(
        replace_ref, insert_ref,
        "union-variant ref anchors share one homographed p#"
    );
    if !SymbolMap::is_opaque_p_sym(replace_ref.as_str()) {
        return;
    }
    let wire = map
        .resolve_cap_param(
            CatalogScope::qualified("proof"),
            "Document",
            "document_edit_v2",
            replace_ref.as_str(),
            cap,
        )
        .expect("homographed ref p# must resolve on document_edit_v2 invoke");
    assert!(
        wire.ends_with(".ref"),
        "expected a declared ref param path, got {wire}"
    );
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
