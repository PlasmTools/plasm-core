//! Catalog obtainability and complete Python declaration coverage.
use crate::prompt_render::python::{prepare_python_teaching_wave, PythonTeachingWave};
use std::collections::HashSet;

use crate::schema::{CapabilityKind, InputFieldSchema};
use crate::{FieldType, SchemaError, ValueWireFormat, CGS};

/// Validate expression-surface invariants: entity/capability graph, scope encodability,
/// per-entity line witnesses, and per-capability coverage.
pub fn validate_cgs_expression_surface(cgs: &CGS) -> Result<(), SchemaError> {
    validate_every_entity_has_capability(cgs)?;
    validate_query_search_scope_params_encodable(cgs)?;
    validate_python_surface(cgs)?;
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
        FieldType::String | FieldType::Uuid | FieldType::DigitId => true,
        FieldType::Integer | FieldType::Number | FieldType::Money => true,
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
        for f in cap.scope_params() {
            if !f.required {
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

fn python_wave(cgs: &CGS) -> Result<PythonTeachingWave, SchemaError> {
    let entry = cgs.entry_id.as_deref().unwrap_or("local");
    let entities = cgs
        .entities
        .keys()
        .map(|key| key.as_str())
        .collect::<Vec<_>>();
    let exposure = crate::TeachingExposureSession::new(cgs, entry, &entities);
    prepare_python_teaching_wave(&exposure, &Default::default()).map_err(|detail| {
        SchemaError::EntityExpressionIncomplete {
            entity: "<catalog>".into(),
            detail,
        }
    })
}

fn validate_python_surface(cgs: &CGS) -> Result<(), SchemaError> {
    let mut obtainable = HashSet::new();
    let roots = cgs
        .capabilities
        .values()
        .filter(|cap| {
            matches!(
                cap.kind,
                CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
            ) || !cap.requires_receiver()
        })
        .collect::<Vec<_>>();
    let root_domains = roots
        .iter()
        .map(|cap| cap.domain.as_str())
        .collect::<HashSet<_>>();
    for cap in roots {
        if let Some(output) = &cap.output_schema {
            match &output.output_type {
                crate::OutputType::Entity { entity_type }
                | crate::OutputType::Collection { entity_type, .. } => {
                    obtainable.insert(entity_type.to_string());
                }
                _ => {}
            }
        } else {
            obtainable.insert(cap.domain.to_string());
        }
    }
    loop {
        let before = obtainable.len();
        for (name, entity) in &cgs.entities {
            if obtainable.contains(name.as_str()) {
                for relation in entity.relations.values() {
                    obtainable.insert(relation.target_resource.to_string());
                }
            }
        }
        if obtainable.len() == before {
            break;
        }
    }
    for (name, entity) in &cgs.entities {
        if !entity.abstract_entity
            && !root_domains.contains(name.as_str())
            && !obtainable.contains(name.as_str())
        {
            return Err(SchemaError::EntityExpressionIncomplete {
                entity: name.to_string(),
                detail: "No declared root capability or relation produces a receiver; add a get, query, search, create or declared entity output.".into(),
            });
        }
    }
    let wave = python_wave(cgs)?;
    let covered = wave
        .capabilities
        .iter()
        .filter(|cap| cap.unavailable.is_none() && cap.signature.is_some())
        .map(|cap| cap.capability.as_str())
        .collect::<HashSet<_>>();
    let uncovered = cgs
        .capabilities
        .iter()
        .filter(|(name, _)| !covered.contains(name.as_str()))
        .map(|(name, cap)| (name.to_string(), cap.domain.to_string()))
        .collect::<Vec<_>>();
    if !uncovered.is_empty() {
        return Err(SchemaError::CapabilityCoverageIncomplete { uncovered });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn uncovered_capabilities(cgs: &CGS) -> Vec<(String, String)> {
    let wave = python_wave(cgs).unwrap();
    let covered = wave
        .capabilities
        .iter()
        .filter(|cap| cap.unavailable.is_none() && cap.signature.is_some())
        .map(|cap| cap.capability.as_str())
        .collect::<HashSet<_>>();
    cgs.capabilities
        .iter()
        .filter(|(name, _)| !covered.contains(name.as_str()))
        .map(|(name, cap)| (name.to_string(), cap.domain.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use std::path::Path;

    #[test]
    fn matrix_fixtures_validate_expression_surface() {
        for dir in [
            "../../fixtures/schemas/plasm_language_matrix",
            "../../fixtures/schemas/plasm_language_matrix_views",
            "../../fixtures/schemas/plasm_prompt_matrix",
            "../../fixtures/schemas/overshow_tools",
        ] {
            let p = Path::new(dir);
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or(dir);
            let cgs = load_schema_dir(p).unwrap_or_else(|e| panic!("{name}: load: {e}"));
            validate_cgs_expression_surface(&cgs)
                .unwrap_or_else(|e| panic!("{name}: expression-surface: {e}"));
        }
    }

    #[test]
    fn language_matrix_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(p).expect("plasm_language_matrix");
        let missing = uncovered_capabilities(&cgs);
        for (cap, ent) in &missing {
            eprintln!("  uncovered: {cap} on {ent}");
        }
        assert!(missing.is_empty(), "uncovered capabilities: {missing:?}");
    }

    #[test]
    fn matrix_fixtures_validate_named_expression_surface() {
        for dir in [
            "../../fixtures/schemas/plasm_language_matrix",
            "../../fixtures/schemas/plasm_language_matrix_views",
            "../../fixtures/schemas/plasm_prompt_matrix",
            "../../fixtures/schemas/overshow_tools",
        ] {
            let p = Path::new(dir);
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
    fn python_teaching_covers_secondary_get_without_native_alias() {
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

        cgs.validate()
            .expect("secondary Get has its own Python method symbol");
        assert!(uncovered_capabilities(&cgs).is_empty());
    }

    #[test]
    fn prompt_matrix_teaching_bundle_covers_all_capabilities() {
        let p = Path::new("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = load_schema_dir(p).expect("plasm_prompt_matrix");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "Teaching bundle should witness every capability: {missing:?}"
        );
    }

    /// WS-R3′ end-to-end: a capability whose inputs carry `values:` constraints and cross-field rules
    /// (`min` / `min_length` / `at_least_one`) must remain **teachable** — the teaching-surface
    /// `$` placeholders and unlisted optional fields no longer trip scalar constraint enforcement — so the
    /// fixture validates and `account_update` is witnessed. This locks the reconciliation: the
    /// empty-teaching-block panic was rooted in placeholder-blind constraint enforcement, not
    /// unobtainability.
    #[test]
    fn validated_input_update_is_teachable_and_covered() {
        let p = Path::new("../../fixtures/schemas/capability_with_input");
        if !p.exists() {
            return;
        }
        let cgs = load_schema_dir(p).expect("capability_with_input validates end-to-end");
        // `load_schema_dir` runs `CGS::validate` (expression-surface + coverage); reaching here
        // proves the validated-input update synthesized at least one teaching line.
        validate_cgs_expression_surface(&cgs).expect("expression surface");
        let missing = uncovered_capabilities(&cgs);
        assert!(
            missing.is_empty(),
            "validated-input `account_update` must be witnessed; uncovered: {missing:?}"
        );
    }

    /// Packed plugins set [`CGS::entry_id`] to the directory name; exposure keys must stay aligned.
    #[test]
    fn language_matrix_expression_surface_validate_with_packed_entry_id() {
        let p = Path::new("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs = load_schema_dir(p).expect("plasm_language_matrix");
        cgs.bind_registry_entry_id("langmatrix");
        validate_cgs_expression_surface(&cgs).unwrap_or_else(|e| {
            panic!("validate_cgs_expression_surface(langmatrix, entry_id=langmatrix): {e}");
        });
    }
}
