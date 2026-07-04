//! CGS rules so schemas cannot load unless every entity and capability is teachable on the typed
//! expression surface.
//!
//! Invoked from [`CGS::validate`](crate::schema::CGS::validate). Structural checks run first; witness +
//! capability coverage share **one** [`crate::prompt_render::render_teaching_prompt_bundle`] (same synthesis as eval teaching table).

use std::collections::{HashMap, HashSet};

use crate::prompt_render::{render_teaching_prompt_bundle_for_validation, TeachingPromptModel};
use crate::schema::{CapabilityKind, InputFieldSchema, ParameterRole};
use crate::{FieldType, SchemaError, ValueWireFormat, CGS};

/// Validate expression-surface invariants: entity/capability graph, scope encodability,
/// per-entity line witnesses, and per-capability coverage.
pub fn validate_cgs_expression_surface(cgs: &CGS) -> Result<(), SchemaError> {
    validate_every_entity_has_capability(cgs)?;
    validate_query_search_scope_params_encodable(cgs)?;
    validate_expression_surface_from_single_bundle(cgs)?;
    Ok(())
}

fn validate_every_entity_has_capability(cgs: &CGS) -> Result<(), SchemaError> {
    for (entity_name, ent) in &cgs.entities {
        if ent.abstract_entity {
            continue;
        }
        let has = cgs.capabilities.values().any(|c| c.domain == *entity_name);
        if !has {
            return Err(SchemaError::EntityWithoutCapability {
                entity: entity_name.to_string(),
            });
        }
    }
    Ok(())
}

fn scope_param_encodable(cgs: &CGS, f: &InputFieldSchema) -> bool {
    let Ok(nv) = f.named_value(cgs) else {
        return false;
    };
    match &nv.field_type {
        FieldType::EntityRef { .. } => true,
        FieldType::String | FieldType::Uuid => true,
        FieldType::Integer | FieldType::Number => true,
        FieldType::Boolean => true,
        FieldType::Select | FieldType::MultiSelect => {
            nv.allowed_values.as_ref().is_some_and(|v| !v.is_empty())
        }
        FieldType::Date => matches!(nv.value_format, Some(ValueWireFormat::Temporal(_))),
        FieldType::Json | FieldType::Array | FieldType::Blob => false,
    }
}

fn validate_query_search_scope_params_encodable(cgs: &CGS) -> Result<(), SchemaError> {
    for (cap_name, cap) in &cgs.capabilities {
        if !matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
            continue;
        }
        let Some(fields) = cap.object_params() else {
            continue;
        };
        for f in fields {
            if !f.required {
                continue;
            }
            if f.role != Some(ParameterRole::Scope) {
                continue;
            }
            if !scope_param_encodable(cgs, f) {
                return Err(SchemaError::ScopeParameterNotEncodable {
                    capability: cap_name.to_string(),
                    parameter: f.name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// One [`render_teaching_prompt_bundle`] pass for witness + capability coverage (was ~2× synthesis).
fn validate_expression_surface_from_single_bundle(cgs: &CGS) -> Result<(), SchemaError> {
    let bundle = render_teaching_prompt_bundle_for_validation(cgs);
    validate_expression_witnesses_from_model(cgs, &bundle.model)?;
    let missing = collect_uncovered_capabilities_with_model(cgs, &bundle.model);
    if !missing.is_empty() {
        return Err(SchemaError::CapabilityCoverageIncomplete { uncovered: missing });
    }
    Ok(())
}

fn validate_expression_witnesses_from_model(
    cgs: &CGS,
    model: &TeachingPromptModel,
) -> Result<(), SchemaError> {
    let mut line_counts: HashMap<&str, usize> = HashMap::new();
    for ep in &model.entities {
        line_counts.insert(ep.entity.as_str(), ep.lines.len());
    }
    for (entity_name, ent) in &cgs.entities {
        if ent.abstract_entity {
            continue;
        }
        let n = line_counts.get(entity_name.as_str()).copied().unwrap_or(0);
        if n == 0 {
            return Err(SchemaError::EntityExpressionIncomplete {
                entity: entity_name.to_string(),
                detail: "no type-checked teaching lines could be synthesized for the teaching bundle (see collect_entity_teaching_block / domain_line_valid)".to_string(),
            });
        }
    }
    Ok(())
}

/// Collect capability ids taught by teaching lines, using renderer metadata (`source_capability`).
fn covered_capabilities_from_model(cgs: &CGS, model: &TeachingPromptModel) -> HashSet<String> {
    let mut covered = HashSet::new();
    for entity in &model.entities {
        for line in &entity.lines {
            if let Some(cap) = &line.source_capability {
                covered.insert(cap.clone());
            }
        }
    }
    expand_expression_family_coverage(cgs, &mut covered);
    expand_query_get_domain_symmetry(cgs, &mut covered);
    expand_query_only_entity_coverage(cgs, model, &mut covered);
    covered
}

/// `Get`/`Query`/`Search` surface forms are capability-agnostic per domain — one taught cap of a
/// kind implies every cap of that kind on the same entity row type is covered.
fn expand_expression_family_coverage(cgs: &CGS, covered: &mut HashSet<String>) {
    let domains_by_kind: HashSet<(String, CapabilityKind)> = covered
        .iter()
        .filter_map(|cap_name| cgs.get_capability(cap_name))
        .filter(|cap| {
            matches!(
                cap.kind,
                CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
            )
        })
        .map(|cap| (cap.domain.to_string(), cap.kind))
        .collect();
    for (domain, kind) in domains_by_kind {
        for cap in cgs.find_capabilities(domain.as_str(), kind) {
            covered.insert(cap.name.to_string());
        }
    }
}

/// Fetch-by-id and scoped list queries are compositional on the same row type — cover `Get` when
/// any `Query` is taught (and symmetrically cover `Query` when any `Get` witness is present).
fn expand_query_get_domain_symmetry(cgs: &CGS, covered: &mut HashSet<String>) {
    let domains_with_query = domains_for_covered_kind(cgs, covered, CapabilityKind::Query);
    let domains_with_get = domains_for_covered_kind(cgs, covered, CapabilityKind::Get);
    insert_all_capabilities_on_domains(cgs, covered, domains_with_query, CapabilityKind::Get);
    insert_all_capabilities_on_domains(cgs, covered, domains_with_get, CapabilityKind::Query);
}

/// Query-only entities: teaching lines without a `Get` witness still imply scoped list-query coverage.
fn expand_query_only_entity_coverage(
    cgs: &CGS,
    model: &TeachingPromptModel,
    covered: &mut HashSet<String>,
) {
    for ep in &model.entities {
        if ep.lines.is_empty() {
            continue;
        }
        let domain_has_get_witness = ep.lines.iter().any(|line| {
            line.source_capability
                .as_ref()
                .and_then(|name| cgs.get_capability(name.as_str()))
                .is_some_and(|cap| cap.kind == CapabilityKind::Get)
        });
        if domain_has_get_witness {
            continue;
        }
        insert_all_capabilities_on_domains(
            cgs,
            covered,
            HashSet::from([ep.entity.to_string()]),
            CapabilityKind::Query,
        );
    }
}

fn domains_for_covered_kind(
    cgs: &CGS,
    covered: &HashSet<String>,
    kind: CapabilityKind,
) -> HashSet<String> {
    covered
        .iter()
        .filter_map(|cap_name| cgs.get_capability(cap_name))
        .filter(|cap| cap.kind == kind)
        .map(|cap| cap.domain.to_string())
        .collect()
}

fn insert_all_capabilities_on_domains(
    cgs: &CGS,
    covered: &mut HashSet<String>,
    domains: HashSet<String>,
    kind: CapabilityKind,
) {
    for domain in domains {
        for cap in cgs.find_capabilities(domain.as_str(), kind) {
            covered.insert(cap.name.to_string());
        }
    }
}

fn collect_uncovered_capabilities_with_model(
    cgs: &CGS,
    model: &TeachingPromptModel,
) -> Vec<(String, String)> {
    let covered = covered_capabilities_from_model(cgs, model);
    let mut missing = Vec::new();
    for (cap_name, cap) in &cgs.capabilities {
        let Some(ent) = cgs.get_entity(cap.domain.as_str()) else {
            continue;
        };
        if ent.abstract_entity {
            continue;
        }
        if !covered.contains(cap_name.as_str()) {
            missing.push((cap_name.to_string(), cap.domain.to_string()));
        }
    }
    missing
}

/// Strict per-capability coverage check (for tests). Returns uncovered capability names.
#[cfg(test)]
pub(crate) fn uncovered_capabilities(cgs: &CGS) -> Vec<(String, String)> {
    let bundle = render_teaching_prompt_bundle_for_validation(cgs);
    collect_uncovered_capabilities_with_model(cgs, &bundle.model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::SchemaError;
    use std::path::{Path, PathBuf};

    fn iter_api_catalog_dirs(root: &Path) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return dirs;
        };
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("domain.yaml").is_file() && path.join("mappings.yaml").is_file() {
                dirs.push(path);
            }
        }
        dirs.sort();
        dirs
    }

    #[test]
    fn all_apis_validate_expression_surface() {
        let apis_root = Path::new("../../apis");
        if !apis_root.is_dir() {
            return;
        }
        let dirs = iter_api_catalog_dirs(apis_root);
        assert!(
            !dirs.is_empty(),
            "expected at least one API catalog under {}",
            apis_root.display()
        );
        let mut failures = Vec::new();
        for p in &dirs {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?");
            match load_schema_dir(p) {
                Ok(cgs) => {
                    if let Err(e) = validate_cgs_expression_surface(&cgs) {
                        failures.push(format!("{name}: {e}"));
                    }
                }
                Err(e) => failures.push(format!("{name}: load: {e}")),
            }
        }
        if !failures.is_empty() {
            for f in &failures {
                eprintln!("FAIL {f}");
            }
            panic!(
                "{} of {} API catalog(s) failed expression-surface validation",
                failures.len(),
                dirs.len()
            );
        }
    }

    #[test]
    fn bundled_github_capability_coverage_report() {
        let p = Path::new("../../apis/github");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("github");
        let missing = uncovered_capabilities(&cgs);
        for (cap, ent) in &missing {
            eprintln!("  uncovered: {cap} on {ent}");
        }
        assert!(missing.is_empty(), "uncovered capabilities: {missing:?}");
    }

    #[test]
    fn bundled_github_petstore_clickup_validate_expression_surface() {
        for dir in [
            "../../apis/github",
            "../../fixtures/schemas/petstore",
            "../../apis/clickup",
            "../../apis/jira",
            "../../apis/linear",
            "../../apis/discord",
            "../../fixtures/schemas/plasm_prompt_matrix",
        ] {
            let p = Path::new(dir);
            if !p.exists() {
                continue;
            }
            let cgs = load_schema_dir(p).expect(dir);
            validate_cgs_expression_surface(&cgs).unwrap_or_else(|e| {
                panic!(
                    "validate_cgs_expression_surface failed for {}: {e}",
                    p.display()
                );
            });
        }
    }

    #[test]
    fn linear_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../apis/linear");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("linear");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability (GraphQL id binding counts as pathful): {missing:?}"
        );
    }

    #[test]
    fn slack_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../apis/slack");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("slack");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability: {missing:?}"
        );
    }

    #[test]
    fn discord_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../apis/discord");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("discord");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability: {missing:?}"
        );
    }

    #[test]
    fn overshow_tools_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../fixtures/schemas/overshow_tools");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("overshow_tools");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability for overshow_tools fixture: {missing:?}"
        );
    }

    #[test]
    fn validate_fails_when_teaching_bundle_omits_a_declared_capability() {
        let p = Path::new("../../fixtures/schemas/overshow_tools");
        if !p.exists() {
            return;
        }
        let mut cgs = load_schema_dir(p).expect("overshow_tools");
        let mut extra_get = cgs
            .capabilities
            .get("capture_item_get")
            .expect("capture_item_get")
            .clone();
        extra_get.name = "capture_item_get_secondary".into();
        let extra_key = extra_get.name.clone();
        cgs.capabilities.insert(extra_key.clone(), extra_get);

        let err = cgs
            .validate()
            .expect_err("duplicate get should fail strict teaching-bundle capability coverage");
        assert!(
            matches!(
                err,
                SchemaError::CapabilityCoverageIncomplete { ref uncovered }
                if uncovered.iter().any(|(cap, ent)| cap == extra_key.as_str() && ent == "CaptureItem")
            ),
            "expected strict coverage failure to mention synthetic capability; got: {err:?}"
        );
    }

    #[test]
    fn proof_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../apis/proof");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("proof");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability: {missing:?}"
        );
    }

    /// Packed plugins set [`CGS::entry_id`] to the directory name; exposure keys must stay aligned.
    #[test]
    fn proof_expression_surface_validate_with_packed_entry_id() {
        let p = Path::new("../../apis/proof");
        if !p.exists() {
            return;
        }
        let mut cgs = load_schema_dir(p).expect("proof");
        cgs.entry_id = Some("proof".to_string());
        validate_cgs_expression_surface(&cgs).unwrap_or_else(|e| {
            panic!("validate_cgs_expression_surface(proof, entry_id=proof): {e}");
        });
    }
}
