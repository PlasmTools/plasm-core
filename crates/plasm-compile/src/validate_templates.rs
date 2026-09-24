//! Pack-time CGS capability template + view validation.

use plasm_core::CapabilitySchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    emit_paginated_list_missing_cml_pagination_warnings, parse_capability_template,
    template_pagination, template_var_names, CapabilityTemplate, CmlError, PaginationConfig,
};

/// Ensure every capability's CML mapping template parses (HTTP or EVM transport).
///
/// Call after loading a [`plasm_core::CGS`] so invalid templates fail at validation time
/// instead of first execution.
///
/// Also rejects **fabricated wire params**: every declared capability parameter must appear
/// as a CML `var` (path/query/body/headers/multipart) or as a pagination param key, unless
/// the template uses the aggregate `input` body var (params splat into env) or `transport: view`
/// (params bind via view scope / node binds, not this HTTP template).
/// Parsed, validated capability request recipes for one exact CGS revision.
///
/// This is a portable pack artifact. Runtime request preparation evaluates these
/// typed recipes; it must never parse `CapabilityTemplateJson` again.
///
/// Trusted catalogs cannot be deserialized directly:
/// ```compile_fail
/// let _: plasm_compile::CompiledCatalog = serde_json::from_slice(b"{}").unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompiledCatalog {
    /// Registry identity supplied by the validated CGS, never by artifact bytes.
    #[serde(skip)]
    entry_id: Option<String>,
    cgs_hash: String,
    capabilities: BTreeMap<String, CapabilityTemplate>,
    conflict_rules: BTreeMap<String, Vec<plasm_core::ConflictRule>>,
}

#[derive(Deserialize)]
struct CompiledCatalogArtifact {
    cgs_hash: String,
    capabilities: BTreeMap<String, CapabilityTemplate>,
    conflict_rules: BTreeMap<String, Vec<plasm_core::ConflictRule>>,
}

impl CompiledCatalog {
    /// Decode untrusted artifact bytes and construct a trusted catalog only after
    /// exact revision and capability-set validation.
    pub fn decode_artifact(bytes: &[u8], cgs: &plasm_core::CGS) -> Result<Self, CmlError> {
        let artifact: CompiledCatalogArtifact =
            serde_json::from_slice(bytes).map_err(|error| CmlError::InvalidTemplate {
                message: format!("decode compiled request recipes: {error}"),
            })?;
        let compiled = Self {
            entry_id: cgs.entry_id.clone(),
            cgs_hash: artifact.cgs_hash,
            capabilities: artifact.capabilities,
            conflict_rules: artifact.conflict_rules,
        };
        compiled.validate_against(cgs)?;
        Ok(compiled)
    }

    pub fn entry_id(&self) -> Option<&str> {
        self.entry_id.as_deref()
    }

    pub fn cgs_hash(&self) -> &str {
        &self.cgs_hash
    }

    pub fn capability(&self, name: &str) -> Result<&CapabilityTemplate, CmlError> {
        self.capabilities
            .get(name)
            .ok_or_else(|| CmlError::InvalidTemplate {
                message: format!("compiled catalog has no request recipe for `{name}`"),
            })
    }

    pub fn conflict_rules(&self, name: &str) -> Result<&[plasm_core::ConflictRule], CmlError> {
        self.conflict_rules
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| CmlError::InvalidTemplate {
                message: format!("compiled catalog has no conflict contract for `{name}`"),
            })
    }

    pub fn validate_against(&self, cgs: &plasm_core::CGS) -> Result<(), CmlError> {
        crate::embed_target_decoder::validate_embedded_identity_contracts(cgs)?;
        let actual_hash = cgs.catalog_cgs_hash_hex();
        if self.cgs_hash != actual_hash {
            return Err(CmlError::InvalidTemplate {
                message: format!(
                    "compiled request recipes target CGS {}, loaded CGS is {actual_hash}",
                    self.cgs_hash
                ),
            });
        }
        let expected = cgs
            .capabilities
            .iter()
            .filter_map(|(name, capability)| capability.derived.is_none().then_some(name.as_str()))
            .collect::<std::collections::BTreeSet<_>>();
        let actual = self
            .capabilities
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if expected != actual {
            return Err(CmlError::InvalidTemplate {
                message: "compiled request recipe capability set does not match the CGS".into(),
            });
        }
        let conflict_actual = self
            .conflict_rules
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if expected != conflict_actual {
            return Err(CmlError::InvalidTemplate {
                message: "compiled conflict contract set does not match the CGS".into(),
            });
        }
        Ok(())
    }
}

/// Load the checksummed compiled-recipe artifact and bind it to the exact CGS.
pub fn load_compiled_catalog_artifact(
    dir: &std::path::Path,
    manifest: &plasm_core::catalog_il::CatalogManifest,
    cgs: &plasm_core::CGS,
) -> Result<CompiledCatalog, CmlError> {
    manifest
        .validate_format()
        .map_err(|message| CmlError::InvalidTemplate { message })?;
    let bytes = std::fs::read(dir.join(&manifest.recipes_json)).map_err(|error| {
        CmlError::InvalidTemplate {
            message: format!("read compiled request recipes: {error}"),
        }
    })?;
    if plasm_core::catalog_discovery::content_hash(&bytes) != manifest.recipes_hash {
        return Err(CmlError::InvalidTemplate {
            message: "compiled request recipe digest mismatch".into(),
        });
    }
    CompiledCatalog::decode_artifact(&bytes, cgs)
}

pub fn compile_cgs_capability_templates(
    cgs: &plasm_core::CGS,
) -> Result<CompiledCatalog, CmlError> {
    let capabilities = compile_capability_templates(cgs)?;
    let compiled = CompiledCatalog {
        entry_id: cgs.entry_id.clone(),
        cgs_hash: cgs.catalog_cgs_hash_hex(),
        capabilities,
        conflict_rules: cgs
            .capabilities
            .iter()
            .filter(|(_, capability)| capability.derived.is_none())
            .map(|(name, capability)| {
                let rules = capability
                    .require_mapping()
                    .map(|mapping| {
                        plasm_core::conflict_rules_from_mapping_template(&mapping.template.0)
                    })
                    .unwrap_or_default();
                (name.to_string(), rules)
            })
            .collect(),
    };
    compiled.validate_against(cgs)?;
    Ok(compiled)
}

fn compile_capability_templates(
    cgs: &plasm_core::CGS,
) -> Result<BTreeMap<String, CapabilityTemplate>, CmlError> {
    crate::embed_target_decoder::validate_embedded_identity_contracts(cgs)?;
    let mut capabilities = BTreeMap::new();
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
        if matches!(template, CapabilityTemplate::CredentialBind(_))
            && !matches!(
                cap.kind,
                plasm_core::CapabilityKind::Create | plasm_core::CapabilityKind::Action
            )
        {
            return Err(CmlError::InvalidTemplate { message: format!("capability `{name}`: credential binding requires a create or action capability") });
        }
        validate_capability_params_wired_in_cml(cgs, name, cap, &template)?;
        validate_capability_path_vars_projectable(cgs, cap)?;
        capabilities.insert(name.to_string(), template);
    }
    emit_paginated_list_missing_cml_pagination_warnings(cgs);
    Ok(capabilities)
}

pub fn validate_cgs_capability_templates(cgs: &plasm_core::CGS) -> Result<(), CmlError> {
    compile_capability_templates(cgs).map(|_| ())
}

/// Every CML path / GraphQL identity var must be projectable from the domain entity's
/// identity wires (`id` / `id_field` / `key_vars`, plus single-path-var alias for non-Create)
/// or declared as a capability input — invented `{entity}_id` transport names fail closed.
fn validate_capability_path_vars_projectable(
    cgs: &plasm_core::CGS,
    cap: &CapabilitySchema,
) -> Result<(), CmlError> {
    plasm_core::prove_path_env_coverage_in_cgs(cgs, cap).map_err(|e| CmlError::InvalidTemplate {
        message: e.to_string(),
    })
}

/// Fail closed when a `pagination.params` key is also a CML template var
/// (path/query/body/headers/multipart). Dual-wire races the driver against
/// manual exists/var fields and is forbidden.
pub(crate) fn forbid_pagination_dual_wire(
    name: &str,
    template: &CapabilityTemplate,
) -> Result<(), CmlError> {
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
    cgs: &plasm_core::CGS,
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

    // A compound entity scope is consumed through its expanded identity keys.
    // Require every key: consuming only part of an identity is not a wired scope.
    for scope in cap.scope_params() {
        let Ok(value) = scope.named_value(cgs) else {
            continue;
        };
        let plasm_core::FieldType::EntityRef { target, .. } = &value.field_type else {
            continue;
        };
        let Some(entity) = cgs.get_entity(target) else {
            continue;
        };
        if entity.key_vars.len() > 1
            && entity
                .key_vars
                .iter()
                .all(|key| wired.contains(key.as_str()))
        {
            wired.insert(scope.name.to_string());
        }
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
            if node.traverse.is_some() {
                if !node.capability.is_empty() || !node.bind.is_empty() || node.when.is_some() {
                    return Err(CmlError::InvalidTemplate { message: format!("view `{view_key}` traversal `{}` cannot also declare capability, bind, or when", node.id) });
                }
                cgs.view_node_entity(view, &node.id)
                    .map_err(|message| CmlError::InvalidTemplate { message })?;
                prior_nodes.insert(node.id.clone());
                continue;
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
            if let ViewRelationBinding::NodeUnionRows { nodes } = &spec.binding {
                if nodes.is_empty() || spec.cardinality != plasm_core::Cardinality::Many {
                    return Err(CmlError::InvalidTemplate { message: format!("view `{view_key}` identity union needs nonempty nodes and a many relation") });
                }
            }
            let nodes: Vec<&str> = match &spec.binding {
                ViewRelationBinding::FirstNodeRowWhere { node, .. }
                | ViewRelationBinding::NodeRowsWhere { node, .. }
                | ViewRelationBinding::NodeAllRows { node }
                | ViewRelationBinding::NodeSingleRow { node } => vec![node],
                ViewRelationBinding::NodeUnionRows { nodes } => {
                    nodes.iter().map(String::as_str).collect()
                }
            };
            for node in nodes {
                if !all_node_ids.contains(node) {
                    return Err(CmlError::InvalidTemplate {
                        message: format!(
                            "view `{view_key}` relation `{}` references unknown node `{node}`",
                            spec.relation
                        ),
                    });
                }
                let entity = cgs
                    .view_node_entity(view, node)
                    .map_err(|message| CmlError::InvalidTemplate { message })?;
                if entity != spec.target {
                    return Err(CmlError::InvalidTemplate { message: format!("view `{view_key}` relation `{}` expects {}, node `{node}` produces {entity}", spec.relation, spec.target) });
                }
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
    use crate::{compile_operation, CmlEnv, CompiledOperation};

    fn commit_matrix_cgs() -> plasm_core::CGS {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        load_schema(&root.join("../../fixtures/schemas/repository_commit_matrix"))
            .expect("load repository_commit_matrix")
    }

    fn compile_commit_query_path(repository: &str) -> String {
        let cgs = commit_matrix_cgs();
        let cap = cgs
            .get_capability("commit_query")
            .expect("missing capability commit_query");
        let mut env = CmlEnv::new();
        env.insert(
            "repository".to_string(),
            Value::String(repository.to_string()),
        );
        apply_entity_ref_scope_splat(&mut env, &cgs, cap).expect("scope splat");
        let template =
            parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
                .unwrap_or_else(|e| panic!("parse commit_query: {e}"));
        let CompiledOperation::Http(req) = compile_operation(&template, &env)
            .unwrap_or_else(|e| panic!("compile commit_query: {e}"))
        else {
            panic!("commit_query should compile to HTTP");
        };
        req.path
    }

    #[test]
    fn commit_matrix_repository_ref_splats_into_commit_query_path() {
        let path = compile_commit_query_path("ryan-s-roberts/plasm-core");
        assert_eq!(path, "/repositories/ryan-s-roberts/plasm-core/commits");
        assert!(!path.contains("%2F") && !path.contains("//"), "{path}");
    }

    #[test]
    fn commit_matrix_query_provides_same_modeled_fields_as_get() {
        let cgs = commit_matrix_cgs();
        let query = cgs.get_capability("commit_query").expect("commit_query");
        let get = cgs.get_capability("commit_get").expect("commit_get");
        assert_eq!(cgs.effective_provides(query), cgs.effective_provides(get));
    }

    #[test]
    fn repo_get_without_owner_fails_before_malformed_path() {
        let cgs = commit_matrix_cgs();
        let cap = cgs.get_capability("repo_get").expect("repo_get");
        let mut env = CmlEnv::new();
        env.insert("repo".to_string(), Value::String("plasm-core".into()));
        apply_entity_ref_scope_splat(&mut env, &cgs, cap)
            .expect("Get has no entity-ref scope to expand");
        let template = parse_capability_template(&cap.require_mapping().expect("mapping").template)
            .expect("template");
        let error = compile_operation(&template, &env)
            .expect_err("missing owner must not produce a request");
        assert!(error.to_string().contains("owner"), "{error}");
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
    fn validate_rejects_unknown_domain_entity_for_path_env() {
        use plasm_core::schema::{
            CapabilityInputs, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson,
            ParentScopeSchema, ResourceSchema,
        };
        use plasm_core::{CapabilityKind, CGS};

        let mut cgs = CGS::new();
        cgs.add_resource(ResourceSchema {
            name: "Pet".into(),
            description: String::new(),
            id_field: "name".into(),
            id_format: None,
            id_from: None,
            fields: vec![],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: true,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .expect("pet");
        let mut cap = CapabilitySchema::minimal_test();
        cap.name = "orphan_get".into();
        cap.kind = CapabilityKind::Get;
        cap.domain = "Pet".into();
        cap.mapping = Some(CapabilityMapping {
            template: CapabilityTemplateJson(serde_json::json!({
                "method": "GET",
                "path": [
                    {"type": "literal", "value": "x"},
                    {"type": "var", "name": "id"}
                ]
            })),
        });
        cap.inputs = CapabilityInputs {
            scope: ParentScopeSchema(vec![]),
            ..CapabilityInputs::default()
        };
        cgs.add_capability(cap).expect("cap");
        // Corrupt domain after insert — pack validate must fail closed, not skip.
        cgs.capabilities.get_mut("orphan_get").unwrap().domain = "MissingEntity".into();
        let err = validate_cgs_capability_templates(&cgs).expect_err("unknown domain");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown domain") && msg.contains("MissingEntity"),
            "{msg}"
        );
    }

    #[test]
    fn validate_rejects_invented_path_var_not_identity_or_input() {
        use plasm_core::schema::{
            CapabilityInputs, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson,
            ParentScopeSchema, ResourceSchema,
        };
        use plasm_core::{CapabilityKind, CGS};

        let mut cgs = CGS::new();
        cgs.add_resource(ResourceSchema {
            name: "Pet".into(),
            description: String::new(),
            id_field: "name".into(),
            id_format: None,
            id_from: None,
            fields: vec![],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: true,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .expect("pet");
        let mut cap = CapabilitySchema::minimal_test();
        cap.name = "pet_get".into();
        cap.kind = CapabilityKind::Get;
        cap.domain = "Pet".into();
        cap.mapping = Some(CapabilityMapping {
            template: CapabilityTemplateJson(serde_json::json!({
                "method": "GET",
                "path": [
                    {"type": "literal", "value": "owners"},
                    {"type": "var", "name": "owner"},
                    {"type": "literal", "value": "pets"},
                    {"type": "var", "name": "pet_id"}
                ]
            })),
        });
        cap.inputs = CapabilityInputs {
            scope: ParentScopeSchema(vec![]),
            ..CapabilityInputs::default()
        };
        cgs.add_capability(cap).expect("cap");
        let err = validate_cgs_capability_templates(&cgs).expect_err("invent pet_id");
        let msg = err.to_string();
        assert!(
            (msg.contains("pet_id") || msg.contains("owner"))
                && (msg.contains("not projectable") || msg.contains("invented")),
            "{msg}"
        );
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
    fn compound_scope_requires_all_expanded_identity_keys_on_the_wire() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = plasm_core::load_schema_dir(
            &root.join("../../fixtures/schemas/repository_commit_matrix"),
        )
        .expect("fixture");
        validate_cgs_capability_templates(&cgs).expect("compound scope is wired through both keys");
        let cap = &cgs.capabilities["commit_query"];
        let template = parse_capability_template(&serde_json::json!({"method":"GET", "path":[{"type":"var", "name":"owner"}], "response":"bare_list"})).expect("template");
        let err = validate_capability_params_wired_in_cml(&cgs, "commit_query", cap, &template)
            .unwrap_err();
        assert!(err.to_string().contains("repository"), "{err}");
    }

    #[test]
    fn language_matrix_templates_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs =
            plasm_core::load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
                .expect("load");
        validate_cgs_capability_templates(&cgs).expect("templates");
        validate_cgs_views(&cgs).expect("views");
    }

    #[test]
    fn pagination_and_prompt_matrix_templates_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for rel in [
            "../../fixtures/schemas/plasm_pagination_matrix",
            "../../fixtures/schemas/plasm_prompt_matrix",
        ] {
            let cgs = plasm_core::load_schema(&root.join(rel)).expect(rel);
            validate_cgs_capability_templates(&cgs)
                .unwrap_or_else(|e| panic!("{rel} templates must validate: {e}"));
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

    #[test]
    fn compiled_catalog_entry_identity_is_bound_from_cgs_not_artifact() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cgs =
            plasm_core::load_schema_dir(&root.join("../../fixtures/schemas/prerequisite_matrix"))
                .unwrap();
        cgs.bind_registry_entry_id("trusted");
        let compiled = compile_cgs_capability_templates(&cgs).unwrap();
        assert_eq!(compiled.entry_id(), Some("trusted"));
        let mut artifact = serde_json::to_value(&compiled).unwrap();
        assert!(artifact.get("entry_id").is_none());
        artifact["entry_id"] = serde_json::json!("invented");
        let decoded =
            CompiledCatalog::decode_artifact(&serde_json::to_vec(&artifact).unwrap(), &cgs)
                .unwrap();
        assert_eq!(decoded.entry_id(), Some("trusted"));
    }

    #[test]
    fn compiled_catalog_round_trip_is_bound_to_exact_cgs_revision() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs =
            plasm_core::load_schema_dir(&root.join("../../fixtures/schemas/prerequisite_matrix"))
                .expect("load prerequisite matrix");
        let compiled = compile_cgs_capability_templates(&cgs).expect("compile recipes");
        let bytes = serde_json::to_vec(&compiled).expect("encode recipes");
        let decoded = CompiledCatalog::decode_artifact(&bytes, &cgs).expect("decode recipes");
        decoded.validate_against(&cgs).expect("exact revision");

        let mut changed = cgs.clone();
        changed.version += 1;
        let changed = changed.fresh_catalog_digest();
        let error = decoded
            .validate_against(&changed)
            .expect_err("different CGS revision must be rejected");
        assert!(error.to_string().contains("target CGS"), "{error}");
    }
}
