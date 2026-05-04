//! CGS rules so schemas cannot load unless every entity and capability is teachable on the typed
//! expression surface.
//!
//! Invoked from [`CGS::validate`](crate::schema::CGS::validate). Structural checks run first; a
//! witness pass reuses the same line synthesis as DOMAIN ([`crate::prompt_render`]) so we do not
//! silently skip entities or capabilities in prompts.

use std::collections::HashSet;

use crate::prompt_render::{render_domain_prompt_bundle, RenderConfig};
use crate::schema::{CapabilityKind, InputFieldSchema, ParameterRole};
use crate::symbol_tuning::symbol_map_for_prompt;
use crate::{FieldType, SchemaError, ValueWireFormat, CGS};

/// Validate expression-surface invariants: entity/capability graph, scope encodability,
/// per-entity line witnesses, and per-capability coverage.
pub fn validate_cgs_expression_surface(cgs: &CGS) -> Result<(), SchemaError> {
    validate_every_entity_has_capability(cgs)?;
    validate_query_search_scope_params_encodable(cgs)?;
    validate_expression_witnesses(cgs)?;
    validate_per_capability_coverage(cgs)?;
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
    let Ok(nv) = cgs.named_value_for_slot(f) else {
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

fn validate_expression_witnesses(cgs: &CGS) -> Result<(), SchemaError> {
    let map = symbol_map_for_prompt(cgs, crate::symbol_tuning::FocusSpec::All, true);
    for (entity_name, ent) in &cgs.entities {
        if ent.abstract_entity {
            continue;
        }
        let n = crate::prompt_render::domain_example_line_count(
            cgs,
            entity_name.as_str(),
            map.as_ref(),
        );
        if n == 0 {
            return Err(SchemaError::EntityExpressionIncomplete {
                entity: entity_name.to_string(),
                detail: "no type-checked example lines could be synthesized for DOMAIN (see collect_entity_teaching_block / domain_line_valid)".to_string(),
            });
        }
    }
    Ok(())
}

/// Collect capability ids taught by DOMAIN lines, using renderer metadata (`source_capability`).
fn covered_capabilities(cgs: &CGS) -> HashSet<String> {
    let bundle = render_domain_prompt_bundle(cgs, RenderConfig::for_eval(None));
    let mut covered = HashSet::new();
    for entity in &bundle.model.entities {
        for line in &entity.lines {
            if let Some(cap) = &line.source_capability {
                covered.insert(cap.clone());
            }
        }
    }
    // `Get`/`Query`/`Search` expressions are capability-agnostic on the surface:
    // - Get: `Entity(id)` / `Entity(k=v,...)`
    // - Query: `Entity{...}` / `Entity`
    // - Search: `Entity~text`
    // If one capability of that kind is taught for an entity, that expression family is present.
    let mut covered_domains_by_kind: HashSet<(String, CapabilityKind)> = covered
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
    for (entity, kind) in covered_domains_by_kind.drain() {
        for cap in cgs.find_capabilities(entity.as_str(), kind) {
            covered.insert(cap.name.to_string());
        }
    }
    // When any `Query` for an entity is on the DOMAIN surface, treat every `Get` on that entity as
    // covered too — fetch-by-id is compositional on the same row type (no unary `e#($)` required).
    let mut domains_with_query: HashSet<String> = HashSet::new();
    for cap_name in &covered {
        if let Some(cap) = cgs.get_capability(cap_name) {
            if cap.kind == CapabilityKind::Query {
                domains_with_query.insert(cap.domain.to_string());
            }
        }
    }
    for domain in domains_with_query {
        for cap in cgs.find_capabilities(domain.as_str(), CapabilityKind::Get) {
            covered.insert(cap.name.to_string());
        }
    }
    covered
}

fn collect_uncovered_capabilities(cgs: &CGS) -> Vec<(String, String)> {
    let covered = covered_capabilities(cgs);
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

fn validate_per_capability_coverage(cgs: &CGS) -> Result<(), SchemaError> {
    let missing = collect_uncovered_capabilities(cgs);
    if !missing.is_empty() {
        return Err(SchemaError::CapabilityCoverageIncomplete { uncovered: missing });
    }
    Ok(())
}

/// Strict per-capability coverage check (for tests). Returns uncovered capability names.
#[cfg(test)]
pub(crate) fn uncovered_capabilities(cgs: &CGS) -> Vec<(String, String)> {
    collect_uncovered_capabilities(cgs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::SchemaError;
    use std::path::Path;

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
    fn linear_domain_covers_all_capabilities() {
        let p = Path::new("../../apis/linear");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("linear");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "DOMAIN should witness every capability (GraphQL id binding counts as pathful): {missing:?}"
        );
    }

    #[test]
    fn slack_domain_covers_all_capabilities() {
        let p = Path::new("../../apis/slack");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("slack");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "DOMAIN should witness every capability: {missing:?}"
        );
    }

    #[test]
    fn discord_domain_covers_all_capabilities() {
        let p = Path::new("../../apis/discord");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("discord");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "DOMAIN should witness every capability: {missing:?}"
        );
    }

    #[test]
    fn overshow_tools_domain_covers_all_capabilities() {
        let p = Path::new("../../fixtures/schemas/overshow_tools");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("overshow_tools");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "DOMAIN should witness every capability for overshow_tools fixture: {missing:?}"
        );
    }

    #[test]
    fn validate_fails_when_domain_omits_a_declared_capability() {
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
            .expect_err("duplicate get should fail strict DOMAIN capability coverage");
        assert!(
            matches!(
                err,
                SchemaError::CapabilityCoverageIncomplete { ref uncovered }
                if uncovered.iter().any(|(cap, ent)| cap == extra_key.as_str() && ent == "CaptureItem")
            ),
            "expected strict coverage failure to mention synthetic capability; got: {err:?}"
        );
    }
}
