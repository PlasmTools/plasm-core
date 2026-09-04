//! Compilation layer for Plasm: CML, predicate compiler, and decoder DSL.
//!
//! This crate transforms typed predicates into backend-specific requests
//! and provides declarative response decoding.
//!
//! CML template types and transport live in [`plasm_cml`].

pub mod backend_filter;
pub mod capability_pagination;
pub mod decoder;
pub mod embed_decode;
pub mod embed_target_decoder;
pub mod embed_tree;
pub mod error;
pub mod json_path;
pub mod predicate_compiler;

pub use plasm_cml::{
    compile_operation, compile_request, eval_cml, eval_cond, parse_capability_template,
    path_var_names_from_request, template_pagination, template_var_names, AuxiliaryHttpMerge,
    CapabilityTemplate, CmlCond, CmlEnv, CmlExpr, CmlRequest, CmlType, CompiledMultipartBody,
    CompiledMultipartPart, CompiledOperation, CompiledRequest, HttpBodyFormat, HttpMethod,
    HttpResponseDecode, MultipartBodySpec, MultipartPartSpec, PaginationConfig, PaginationLocation,
    PaginationParam, PaginationParamRole, PaginationStop, PaginationStrategyKind,
    PathSegment as CmlPathSegment, ResponsePreprocess, ValidatedPagination, ViewCompiled,
    ViewTemplate,
};

#[cfg(feature = "evm")]
pub use plasm_cml::evm_transport::*;

pub use backend_filter::*;
pub use capability_pagination::{
    emit_paginated_list_missing_cml_pagination_warnings, is_pagination_wire_param,
    paginated_list_missing_cml_pagination_warnings, pagination_contract_validation_errors,
};
pub use decoder::*;
pub use embed_decode::{decode_entities, decode_entities_with_cgs};
pub use embed_target_decoder::entity_decoder_for_from_parent_get_target;
pub use embed_tree::flatten_decoded_embed_descendants;
pub use error::{CompileError, DecodeError};
pub use json_path::path_expr_from_json_segments;
pub use plasm_cml::CmlError;
pub use predicate_compiler::*;

use plasm_core::{CapabilitySchema, QueryExpr, CGS};

/// Canonical compile hook trait objects (shared by `plasm-runtime`).
///
/// [`CmlEnv`] remains a map of [`plasm_core::value::Value`]. Invoke/create IR may hold structured
/// [`plasm_core::InvokeInputPayload`] internally; execution lowers that to [`Value`] before building the
/// env, so plugins see the same wire shapes as the built-in [`compile_operation`] path.
pub type CompileOperationHook =
    dyn Fn(&CapabilityTemplate, &CmlEnv) -> Result<CompiledOperation, CmlError> + Send + Sync;
pub type CompileQueryHook =
    dyn Fn(&QueryExpr, &CGS) -> Result<Option<BackendFilter>, CompileError> + Send + Sync;

/// Ensure every capability's CML mapping template parses (HTTP or EVM transport).
///
/// Call after loading a [`plasm_core::CGS`] so invalid templates fail at validation time
/// instead of first execution.
///
/// Also rejects **fabricated wire params**: every declared capability parameter must appear
/// as a CML `var` (path/query/body/headers/multipart) or as a pagination param key, unless
/// the template uses the aggregate `input` body var (params splat into env) or `transport: view`
/// (params bind via view scope / node binds, not this HTTP template).
pub fn validate_cgs_capability_templates(cgs: &plasm_core::CGS) -> Result<(), CmlError> {
    for (name, cap) in &cgs.capabilities {
        // Domain-authored derived Gets have no CML mapping.
        if cap.derived.is_some() {
            continue;
        }
        let template_json = &cap
            .require_mapping()
            .map_err(|message| CmlError::InvalidTemplate { message })?
            .template
            .0;
        let template =
            parse_capability_template(template_json).map_err(|e| CmlError::InvalidTemplate {
                message: format!("capability `{name}`: {e}"),
            })?;
        let template_text = template_json
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| template_json.to_string());
        plasm_core::bind_wire_validate::validate_bind_wire_refs(
            &template_text,
            &format!("capability `{name}` CML template"),
        )
        .map_err(|e| CmlError::InvalidTemplate {
            message: e.to_string(),
        })?;

        forbid_pagination_dual_wire(name, &template)?;
        validate_capability_params_wired_in_cml(name, cap, &template)?;
    }
    emit_paginated_list_missing_cml_pagination_warnings(cgs);
    Ok(())
}

/// Fail closed when a `pagination.params` key is also a CML template var
/// (path/query/body/headers/multipart). Dual-wire races the driver against
/// manual exists/var fields and is forbidden.
fn forbid_pagination_dual_wire(name: &str, template: &CapabilityTemplate) -> Result<(), CmlError> {
    let Some(pconf) = template_pagination(template) else {
        return Ok(());
    };
    let cml_vars: std::collections::HashSet<String> =
        template_var_names(template).into_iter().collect();
    let mut dual: Vec<&str> = pconf
        .params
        .keys()
        .filter(|k| cml_vars.contains(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    dual.sort_unstable();
    if !dual.is_empty() {
        return Err(CmlError::InvalidTemplate {
            message: format!(
                "capability `{name}`: pagination param(s) [{}] also appear as CML template vars (path/query/body/headers/multipart) — dual-wire is forbidden; remove the manual fields and let `pagination:` drive the wire",
                dual.join(", ")
            ),
        });
    }
    Ok(())
}

/// Every declared capability parameter must appear in the CML template (or pagination keys).
fn validate_capability_params_wired_in_cml(
    name: &str,
    cap: &CapabilitySchema,
    template: &CapabilityTemplate,
) -> Result<(), CmlError> {
    if matches!(template, CapabilityTemplate::View(_)) {
        return Ok(());
    }

    let params: Vec<_> = cap.input_fields().collect();
    if params.is_empty() {
        return Ok(());
    }

    let mut wired: std::collections::HashSet<String> =
        template_var_names(template).into_iter().collect();
    if let Some(pconf) = template_pagination(template) {
        for key in pconf.params.keys() {
            wired.insert(key.clone());
        }
    }
    if wired.contains("input") {
        return Ok(());
    }

    let mut missing: Vec<&str> = params
        .iter()
        .filter(|p| !wired.contains(p.name.as_str()))
        .map(|p| p.name.as_str())
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(CmlError::InvalidTemplate {
            message: format!(
                "capability `{name}`: parameter(s) [{}] are declared in domain.yaml but not referenced in CML (path/query/body/headers/multipart/pagination). Fabricated filters/params that never hit the wire are forbidden — wire them in mappings.yaml or remove them from the capability.",
                missing.join(", ")
            ),
        });
    }
    Ok(())
}

fn validate_view_template_syntax(label: &str, template: &str) -> Result<(), CmlError> {
    if template.len() > 32_768 {
        return Err(CmlError::InvalidTemplate {
            message: format!("{label}: template exceeds 32KiB"),
        });
    }
    minijinja::Environment::new()
        .template_from_str(template)
        .map_err(|e| CmlError::InvalidTemplate {
            message: format!("{label}: {e}"),
        })?;
    Ok(())
}

fn view_node_ids(view: &plasm_core::schema::ViewDefinition) -> indexmap::IndexSet<String> {
    view.nodes.iter().map(|n| n.id.clone()).collect()
}

/// Static validation for CGS `views:` DAGs at catalog load (no expr / HTTP).
pub fn validate_cgs_views(cgs: &plasm_core::CGS) -> Result<(), CmlError> {
    use plasm_core::schema::{ViewOutputBinding, ViewParamBinding, ViewRelationBinding};
    use plasm_core::CapabilityKind;
    use std::collections::HashSet;

    for (view_key, view) in &cgs.views {
        let cap = cgs
            .get_capability(view.capability.as_str())
            .ok_or_else(|| CmlError::InvalidTemplate {
                message: format!(
                    "view `{view_key}` references unknown capability `{}`",
                    view.capability
                ),
            })?;
        let template = parse_capability_template(
            &cap.require_mapping()
                .map_err(|message| CmlError::InvalidTemplate { message })?
                .template,
        )?;
        match &template {
            CapabilityTemplate::View(vt) if vt.view == *view_key => {}
            CapabilityTemplate::View(vt) => {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}`: capability `{}` maps to view `{}`",
                        view.capability, vt.view
                    ),
                });
            }
            _ => {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` capability `{}` must use transport: view",
                        view.capability
                    ),
                });
            }
        }

        if cgs.get_entity(view.entity.as_str()).is_none() {
            return Err(CmlError::InvalidTemplate {
                message: format!("view `{view_key}` targets unknown entity `{}`", view.entity),
            });
        }

        for sp in &view.scope {
            if sp.required && sp.inject.is_some() {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` scope `{}` cannot be both required and inject",
                        sp.name
                    ),
                });
            }
        }

        let all_node_ids = view_node_ids(view);
        let mut seen_node_ids: HashSet<String> = HashSet::new();
        let mut prior_nodes: HashSet<String> = HashSet::new();

        for node in &view.nodes {
            if !seen_node_ids.insert(node.id.clone()) {
                return Err(CmlError::InvalidTemplate {
                    message: format!("view `{view_key}` has duplicate node id `{}`", node.id),
                });
            }
            let node_cap = cgs
                .get_capability(node.capability.as_str())
                .ok_or_else(|| CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` node `{}` references unknown capability `{}`",
                        node.id, node.capability
                    ),
                })?;
            match node_cap.kind {
                CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {}
                other => {
                    return Err(CmlError::InvalidTemplate {
                        message: format!(
                            "view `{view_key}` node `{}`: unsupported capability kind {other:?}",
                            node.id
                        ),
                    });
                }
            }
            let inner_template = parse_capability_template(
                &node_cap
                    .require_mapping()
                    .map_err(|message| CmlError::InvalidTemplate { message })?
                    .template,
            )?;
            if matches!(inner_template, CapabilityTemplate::View(_)) {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` node `{}`: nested view capabilities are not supported",
                        node.id
                    ),
                });
            }

            for (param, binding) in &node.bind {
                match binding {
                    ViewParamBinding::NodeField { node: ref_node, .. } => {
                        if !prior_nodes.contains(ref_node) {
                            return Err(CmlError::InvalidTemplate {
                                message: format!(
                                    "view `{view_key}` node `{}` bind `{param}` references `{ref_node}` before it runs",
                                    node.id
                                ),
                            });
                        }
                    }
                    ViewParamBinding::Computed { template } => {
                        validate_view_template_syntax(
                            &format!(
                                "view `{view_key}` node `{}` bind `{param}` computed",
                                node.id
                            ),
                            template,
                        )?;
                    }
                    _ => {}
                }
            }
            prior_nodes.insert(node.id.clone());
        }

        for (field, binding) in &view.output {
            match binding {
                ViewOutputBinding::NodeRowCount { node }
                | ViewOutputBinding::NodeField { node, .. }
                | ViewOutputBinding::NodeFieldHistogramJson { node, .. }
                | ViewOutputBinding::NodeAnyRowFieldEquals { node, .. }
                | ViewOutputBinding::NodeRowCountPositive { node }
                | ViewOutputBinding::WriteCreated { node }
                | ViewOutputBinding::WriteReused { node }
                | ViewOutputBinding::WriteSkipped { node } => {
                    if !all_node_ids.contains(node) {
                        return Err(CmlError::InvalidTemplate {
                            message: format!(
                                "view `{view_key}` output `{field}` references unknown node `{node}`"
                            ),
                        });
                    }
                }
                ViewOutputBinding::Computed { template } => {
                    validate_view_template_syntax(
                        &format!("view `{view_key}` output `{field}` computed"),
                        template,
                    )?;
                }
                ViewOutputBinding::Scope { .. } => {}
            }
        }

        for spec in &view.relation_outputs {
            let node = match &spec.binding {
                ViewRelationBinding::FirstNodeRowWhere { node, .. }
                | ViewRelationBinding::NodeRowsWhere { node, .. }
                | ViewRelationBinding::NodeAllRows { node }
                | ViewRelationBinding::NodeSingleRow { node } => node,
            };
            if !all_node_ids.contains(node) {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` relation `{}` references unknown node `{node}`",
                        spec.relation
                    ),
                });
            }
            if cgs.get_entity(spec.target.as_str()).is_none() {
                return Err(CmlError::InvalidTemplate {
                    message: format!(
                        "view `{view_key}` relation `{}` targets unknown entity `{}`",
                        spec.relation, spec.target
                    ),
                });
            }
        }
    }

    for (cap_name, cap) in &cgs.capabilities {
        let Some(mapping) = &cap.mapping else {
            continue;
        };
        let Ok(template) = parse_capability_template(&mapping.template) else {
            continue;
        };
        if let CapabilityTemplate::View(vt) = template {
            if !cgs.views.contains_key(vt.view.as_str()) {
                return Err(CmlError::InvalidTemplate {
                    message: format!("capability `{cap_name}` maps to unknown view `{}`", vt.view),
                });
            }
        }
    }

    Ok(())
}

/// Parse one capability template and return its composable pagination stanza, when present.
///
/// Shared by CLI generation and tool-model projection so both surfaces interpret pagination
/// from the exact same CML parsing path.
pub fn pagination_config_for_capability(cap: &CapabilitySchema) -> Option<PaginationConfig> {
    let mapping = cap.mapping.as_ref()?;
    parse_capability_template(&mapping.template)
        .ok()
        .and_then(|template| template_pagination(&template).cloned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use plasm_core::apply_entity_ref_scope_splat;
    use plasm_core::load_schema;
    use plasm_core::value::Value;

    use super::*;

    fn github_cgs() -> plasm_core::CGS {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        load_schema(&root.join("../../apis/github")).expect("load github schema")
    }

    fn compile_github_repo_scoped_path(capability: &str, repository: &str) -> String {
        let cgs = github_cgs();
        let cap = cgs
            .get_capability(capability)
            .unwrap_or_else(|| panic!("missing capability {capability}"));
        let mut env = CmlEnv::new();
        env.insert(
            "repository".to_string(),
            Value::String(repository.to_string()),
        );
        apply_entity_ref_scope_splat(&mut env, &cgs, cap).expect("scope splat");
        let template =
            parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
                .unwrap_or_else(|e| panic!("parse {capability}: {e}"));
        let CompiledOperation::Http(req) = compile_operation(&template, &env)
            .unwrap_or_else(|e| panic!("compile {capability}: {e}"))
        else {
            panic!("{capability} should compile to HTTP");
        };
        req.path
    }

    #[test]
    fn github_repository_ref_splats_into_repo_scoped_list_paths() {
        for (capability, suffix) in [
            ("commit_query", "/commits"),
            ("branch_query", "/branches"),
            ("contributor_query", "/contributors"),
        ] {
            let path = compile_github_repo_scoped_path(capability, "ryan-s-roberts/plasm-core");
            assert_eq!(path, format!("/repos/ryan-s-roberts/plasm-core{suffix}"));
            assert!(
                !path.contains("%2F") && !path.contains("//"),
                "{capability} built malformed path {path}"
            );
        }
    }

    #[test]
    fn github_commit_query_provides_same_modeled_fields_as_get() {
        let cgs = github_cgs();
        let query = cgs.get_capability("commit_query").expect("commit_query");
        let get = cgs.get_capability("commit_get").expect("commit_get");
        assert_eq!(cgs.effective_provides(query), cgs.effective_provides(get));
    }

    #[test]
    fn github_repository_ref_without_owner_fails_before_malformed_path() {
        let cgs = github_cgs();
        let cap = cgs.get_capability("commit_query").expect("commit_query");
        let mut env = CmlEnv::new();
        env.insert("repository".to_string(), Value::String("plasm-core".into()));
        let splat_err = apply_entity_ref_scope_splat(&mut env, &cgs, cap).expect_err("splat");
        assert!(
            splat_err.to_string().contains("cannot normalize")
                || splat_err.to_string().contains("key_vars"),
            "{splat_err}"
        );
    }

    #[test]
    fn matrix_views_validate_at_load() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = plasm_core::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix_views"),
        )
        .expect("load matrix views");
        validate_cgs_capability_templates(&cgs).expect("templates");
        validate_cgs_views(&cgs).expect("views valid");
    }

    #[test]
    fn validate_rejects_capability_param_not_referenced_in_cml() {
        use plasm_core::schema::{InputFieldSchema, InputFieldWire};

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cgs = plasm_core::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix_views"),
        )
        .expect("load matrix views");
        let cap = cgs
            .capabilities
            .get_mut("langitem_query")
            .expect("langitem_query");
        cap.inputs.selection.0.push(InputFieldSchema {
            name: "fabricated_filter".into(),
            wire: InputFieldWire::Registry(
                plasm_core::ValueDomainKey::new("nv_lang_item_title").expect("key"),
            ),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        });
        let err = validate_cgs_capability_templates(&cgs).expect_err("fabricated");
        let msg = err.to_string();
        assert!(
            msg.contains("fabricated_filter") && msg.contains("not referenced in CML"),
            "{msg}"
        );
    }

    #[test]
    fn supervisor_account_password_catalog_validates() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs =
            plasm_core::load_schema(&root.join("../../apis/appworld/supervisor")).expect("load");
        validate_cgs_capability_templates(&cgs).expect("templates");
        validate_cgs_views(&cgs).expect("views");
    }

    #[test]
    fn appworld_amazon_and_gmail_templates_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for rel in ["../../apis/appworld/amazon", "../../apis/appworld/gmail"] {
            let cgs = plasm_core::load_schema(&root.join(rel)).expect(rel);
            validate_cgs_capability_templates(&cgs).unwrap_or_else(|e| {
                panic!("{rel} templates must validate after pagination cutover: {e}")
            });
        }
    }

    #[test]
    fn forbid_pagination_dual_wire_rejects_overlapping_query_vars() {
        let template = parse_capability_template(&serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "items"}],
            "query": {
                "type": "object",
                "fields": [
                    ["page_index", {"type": "var", "name": "page_index"}],
                    ["page_limit", {"type": "var", "name": "page_limit"}]
                ]
            },
            "pagination": {
                "params": {
                    "page_index": {"counter": 0},
                    "page_limit": {"fixed": 20, "role": "page_size"}
                },
                "strategy": "page_number"
            }
        }))
        .expect("parse dual-wire template");
        let err = forbid_pagination_dual_wire("list_items", &template).expect_err("dual-wire");
        let msg = err.to_string();
        assert!(
            msg.contains("dual-wire") && msg.contains("page_index") && msg.contains("page_limit"),
            "{msg}"
        );
    }

    #[test]
    fn forbid_pagination_dual_wire_allows_pagination_only() {
        let template = parse_capability_template(&serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "items"}],
            "query": {
                "type": "object",
                "fields": [
                    ["query", {"type": "var", "name": "query"}]
                ]
            },
            "pagination": {
                "params": {
                    "page_index": {"counter": 0},
                    "page_limit": {"fixed": 20, "role": "page_size"}
                },
                "strategy": "page_number"
            }
        }))
        .expect("parse clean template");
        forbid_pagination_dual_wire("list_items", &template).expect("pagination-only ok");
    }

    #[test]
    fn validate_cgs_views_rejects_duplicate_node_id() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cgs = plasm_core::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix_views"),
        )
        .expect("load matrix views");
        let view = cgs.views.get_mut("lang_digest").expect("view");
        view.nodes.push(view.nodes[0].clone());
        let err = validate_cgs_views(&cgs).expect_err("duplicate node");
        assert!(err.to_string().contains("duplicate node id"), "{err}");
    }

    #[test]
    fn binding_matrix_fixture_rejects_unknown_bind_wire() {
        let err = plasm_core::bind_wire_validate::validate_bind_wire_refs(
            "GET /\nHost: {{ bind.evil_origin }}",
            "capability `schema_query` CML template",
        )
        .expect_err("unknown bind");
        assert!(err.to_string().contains("bind.evil_origin"));
    }
}
