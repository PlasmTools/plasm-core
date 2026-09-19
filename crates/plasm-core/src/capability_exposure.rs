//! Exact teaching exposure for selected business and prerequisite capabilities.

use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
    ExposureSurfaceDelta,
};
use crate::{CapabilityKind, CGS};
use std::collections::BTreeSet;

/// Domain-entity teaching slots (RA-12).
///
/// Reads admit [`CGS::effective_ordered_response_fields`] — the full authored
/// set — so NAPI / `exposeSeeds` cannot shear `[…]` to a read `provides` list.
/// Mutators admit that write's `provides`, plus `id_field` so identity stays
/// bindable when the write omits it.
fn admit_teaching_entity_fields(
    cgs: &CGS,
    cap: &crate::schema::CapabilitySchema,
    entity: &crate::schema::EntityDef,
    entity_key: &ExposureEntityKey,
    surface: &mut ExposureSurface,
) {
    for field in cgs.effective_ordered_response_fields(cap) {
        surface.slots.insert(ExposureSlotKey::EntityField {
            entity: entity_key.clone(),
            field: field.into(),
        });
    }
    surface.slots.insert(ExposureSlotKey::EntityField {
        entity: entity_key.clone(),
        field: entity.id_field.clone(),
    });
}

/// Output-entity teaching slots. Reads take the full authored output field set
/// (RA-12). Mutators keep `provides` (empty `provides` still admits every
/// output field — same as the prior write contract).
fn admit_output_entity_fields(
    cap: &crate::schema::CapabilitySchema,
    output_entity: &crate::schema::EntityDef,
    output_key: &ExposureEntityKey,
    surface: &mut ExposureSurface,
) {
    let read = matches!(
        cap.kind,
        CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
    );
    for field in CGS::default_ordered_entity_field_names(output_entity) {
        if read || cap.provides.is_empty() || cap.provides.iter().any(|name| name == field.as_str())
        {
            surface.slots.insert(ExposureSlotKey::EntityField {
                entity: output_key.clone(),
                field: field.into(),
            });
        }
    }
}

/// Project exactly the selected capability IDs into the teaching surface.
///
/// The caller owns semantic selection and deterministic prerequisite closure.
/// This module only renders that decision: it does not admit sibling reads,
/// entity-wide mutators, or type-compatible producers that were not selected.
/// Explicit entity exposure remains available through
/// [`explicit_entity_capability_surface`].
pub fn selected_capability_surface(
    cgs: &CGS,
    entry_id: &str,
    capabilities: &[String],
) -> Result<ExposureSurfaceDelta, String> {
    let mut surface = ExposureSurface::default();
    for name in capabilities {
        let cap = cgs
            .capabilities
            .get(name.as_str())
            .ok_or_else(|| format!("unknown selected capability {entry_id}/{name}"))?;
        let entity = cgs
            .entities
            .get(cap.domain.as_str())
            .ok_or("selected capability entity missing")?;
        let entity_key = ExposureEntityKey {
            entry_id: entry_id.into(),
            entity: cap.domain.clone(),
        };
        surface.entities.insert(entity_key.clone());
        let capability_key = ExposureCapabilityKey {
            entry_id: entry_id.into(),
            domain: cap.domain.clone(),
            capability: cap.name.clone(),
        };
        surface.capabilities.insert(capability_key.clone());
        for field in cap.input_fields() {
            surface.slots.insert(ExposureSlotKey::CapabilityParam {
                capability: capability_key.clone(),
                param: field.name.clone().into(),
            });
        }
        // RA-12: Get/Query/Search `[…]` is the full authored field set, not `provides`.
        // Mutators still teach that write's `provides`. Identity stays bindable either way.
        admit_teaching_entity_fields(cgs, cap, entity, &entity_key, &mut surface);
        if let Some(output) = &cap.output_schema {
            if let crate::schema::OutputType::Entity { entity_type }
            | crate::schema::OutputType::Collection { entity_type, .. } = &output.output_type
            {
                let output_entity = cgs
                    .entities
                    .get(entity_type.as_str())
                    .ok_or("selected output entity missing")?;
                let output_key = ExposureEntityKey {
                    entry_id: entry_id.into(),
                    entity: entity_type.as_str().into(),
                };
                surface.entities.insert(output_key.clone());
                admit_output_entity_fields(cap, output_entity, &output_key, &mut surface);
            }
        }
    }
    let entities: BTreeSet<_> = surface.entities.iter().cloned().collect();
    for key in &entities {
        let entity = cgs
            .entities
            .get(key.entity.as_str())
            .ok_or("exposed entity missing")?;
        for (name, relation) in &entity.relations {
            if entities.contains(&ExposureEntityKey {
                entry_id: entry_id.into(),
                entity: relation.target_resource.clone(),
            }) {
                surface.slots.insert(ExposureSlotKey::Relation {
                    source: key.clone(),
                    relation: name.clone(),
                });
            }
        }
    }
    Ok(ExposureSurfaceDelta { required: surface })
}

/// Explicit execution exposes capabilities owned by the caller-selected entities.
/// Intent routing must pass selected capability IDs to `selected_capability_surface` instead.
pub fn explicit_entity_capability_surface(
    cgs: &CGS,
    entry: &str,
    entities: &[String],
) -> Result<ExposureSurfaceDelta, String> {
    let capabilities = cgs
        .capabilities
        .values()
        .filter(|capability| {
            entities
                .iter()
                .any(|entity| entity == capability.domain.as_str())
        })
        .map(|capability| capability.name.to_string())
        .collect::<Vec<_>>();
    selected_capability_surface(cgs, entry, &capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::{ExposureSlotKey, ExposureSurfaceDelta};

    fn auth_bearer_search_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/auth_bearer_search")
    }

    #[test]
    fn selected_parent_read_does_not_admit_relation_target_capabilities() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = plasm_core_fixture_load(&root);
        let read = cgs
            .capabilities
            .values()
            .find(|c| c.domain.as_str() == "LangItem" && c.kind == CapabilityKind::Get)
            .unwrap();
        let delta = selected_capability_surface(&cgs, "fixture", &[read.name.to_string()]).unwrap();
        assert!(!delta
            .required
            .entities
            .iter()
            .any(|e| e.entity.as_str() == "LangSummary"));
        assert!(!delta.required.slots.iter().any(|s| matches!(s, ExposureSlotKey::Relation { source, relation } if source.entity.as_str() == "LangItem" && relation.as_str() == "summary")));
        assert_eq!(capability_names(&delta), vec![read.name.as_str()]);
    }
    fn plasm_core_fixture_load(path: &std::path::Path) -> CGS {
        crate::load_schema_dir(path).unwrap()
    }

    #[test]
    fn search_only_selection_does_not_admit_sibling_get() {
        let dir = auth_bearer_search_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let delta = selected_capability_surface(&cgs, "", &["securednote_search".into()])
            .expect("search-only");
        let names: Vec<&str> = delta
            .required
            .capabilities
            .iter()
            .map(|c| c.capability.as_str())
            .collect();
        assert!(
            names.contains(&"securednote_search"),
            "Search must stay: {names:?}"
        );
        assert!(
            !names.contains(&"securednote_get"),
            "unselected Get must stay out: {names:?}"
        );
        assert!(
            !names.contains(&"authsession_login"),
            "read-family close must not pull unrelated mutators: {names:?}"
        );
    }

    #[test]
    fn get_only_selection_does_not_admit_sibling_search() {
        let dir = auth_bearer_search_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let delta =
            selected_capability_surface(&cgs, "", &["securednote_get".into()]).expect("get-only");
        let names: Vec<&str> = delta
            .required
            .capabilities
            .iter()
            .map(|c| c.capability.as_str())
            .collect();
        assert!(
            names.contains(&"securednote_get"),
            "Get must stay: {names:?}"
        );
        assert!(
            !names.contains(&"securednote_search"),
            "unselected Search must stay out: {names:?}"
        );
    }

    fn scoped_query_matrix_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/scoped_query_matrix")
    }

    fn capability_names(delta: &ExposureSurfaceDelta) -> Vec<&str> {
        delta
            .required
            .capabilities
            .iter()
            .map(|c| c.capability.as_str())
            .collect()
    }

    fn entity_names(delta: &ExposureSurfaceDelta) -> Vec<&str> {
        delta
            .required
            .entities
            .iter()
            .map(|e| e.entity.as_str())
            .collect()
    }

    #[test]
    fn query_entity_ref_scope_does_not_admit_unselected_sources_or_siblings() {
        let cgs = load_schema_dir(&scoped_query_matrix_dir()).expect("scoped_query_matrix");
        let delta =
            selected_capability_surface(&cgs, "scoped_query_matrix", &["child_query".into()])
                .expect("child query");
        let names = capability_names(&delta);
        let entities = entity_names(&delta);
        assert!(
            names.contains(&"child_query"),
            "Child Query must stay: {names:?}"
        );
        assert_eq!(names, vec!["child_query"]);
        assert_eq!(entities, vec!["Child"]);
        assert!(
            !names.contains(&"parent_create"),
            "unselected Parent mutator must stay out: {names:?}"
        );
        assert!(
            !names.contains(&"friend_query") && !entities.contains(&"Friend"),
            "unselected Friend must stay untaught: {names:?} {entities:?}"
        );
        assert!(
            !names.contains(&"member_query")
                && !names.contains(&"cohort_get")
                && !entities.contains(&"Cohort"),
            "integer-named scope must not infer an identity target: {names:?} {entities:?}"
        );
    }

    #[test]
    fn query_selection_does_not_admit_seeded_entity_mutators() {
        let cgs = load_schema_dir(&scoped_query_matrix_dir()).expect("scoped_query_matrix");
        let delta =
            selected_capability_surface(&cgs, "scoped_query_matrix", &["child_query".into()])
                .expect("child query");
        let names = capability_names(&delta);
        assert!(
            !names.contains(&"child_settle"),
            "unselected Child mutator must stay out: {names:?}"
        );
        assert!(
            !names.contains(&"parent_create"),
            "identity-scope Parent must not gain writes from Child seed: {names:?}"
        );
    }

    #[test]
    fn mutator_entity_ref_arg_does_not_admit_unselected_target_reads() {
        let cgs = load_schema_dir(&scoped_query_matrix_dir()).expect("scoped_query_matrix");
        let delta =
            selected_capability_surface(&cgs, "scoped_query_matrix", &["child_settle".into()])
                .expect("child settle");
        let names = capability_names(&delta);
        assert!(
            names.contains(&"child_settle"),
            "mutator must stay: {names:?}"
        );
        assert!(
            !names.contains(&"parent_query") && !names.contains(&"parent_get"),
            "unselected Parent reads must stay out: {names:?}"
        );
        assert!(
            !names.contains(&"parent_create") && !names.contains(&"friend_query"),
            "mutator close must not pull Parent writes or Friend: {names:?}"
        );
    }

    fn entity_field_slot_names(delta: &ExposureSurfaceDelta) -> BTreeSet<String> {
        delta
            .required
            .slots
            .iter()
            .filter_map(|s| match s {
                ExposureSlotKey::EntityField { field, .. } => Some(field.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn read_surface_admits_full_authored_fields_not_provides_subset() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/return_projection_teaching");
        let cgs = load_schema_dir(&dir).expect("return_projection_teaching");
        let want =
            CGS::default_ordered_entity_field_names(cgs.get_entity("Notice").expect("Notice"));
        assert_eq!(
            want,
            vec![
                "notice_id".to_string(),
                "author_email".to_string(),
                "body".to_string(),
                "created_at".to_string(),
                "title".to_string(),
            ]
        );
        let delta = selected_capability_surface(&cgs, "", &["notice_get".into()]).expect("get");
        let names = entity_field_slot_names(&delta);
        for field in &want {
            assert!(
                names.contains(field),
                "NAPI/exposeSeeds surface must admit RA-12 field {field}; slots={names:?}"
            );
        }
        assert!(
            names.contains("author_email")
                && names.contains("body")
                && names.contains("created_at"),
            "must not shear to provides [notice_id,title]; slots={names:?}"
        );

        let tx_want = CGS::default_ordered_entity_field_names(
            cgs.get_entity("Transaction").expect("Transaction"),
        );
        assert_eq!(
            tx_want,
            vec![
                "transaction_id".to_string(),
                "amount".to_string(),
                "created_at".to_string(),
                "description".to_string(),
                "private".to_string(),
            ]
        );
        let tx_delta = selected_capability_surface(&cgs, "", &["transaction_get".into()])
            .expect("transaction get");
        let tx_names = entity_field_slot_names(&tx_delta);
        for field in &tx_want {
            assert!(
                tx_names.contains(field),
                "NAPI/exposeSeeds surface must admit RA-12 field {field}; slots={tx_names:?}"
            );
        }
        assert!(
            tx_names.contains("created_at") && tx_names.contains("private"),
            "must not shear to provides [transaction_id,amount,description]; slots={tx_names:?}"
        );
    }

    #[test]
    fn integer_scope_hole_does_not_admit_name_matched_entity() {
        let cgs = load_schema_dir(&scoped_query_matrix_dir()).expect("scoped_query_matrix");
        let delta =
            selected_capability_surface(&cgs, "scoped_query_matrix", &["member_query".into()])
                .expect("member query");
        let names = capability_names(&delta);
        let entities = entity_names(&delta);
        assert!(
            names.contains(&"member_query"),
            "Member Query must stay: {names:?}"
        );
        assert!(
            !names.contains(&"parent_query")
                && !names.contains(&"parent_get")
                && !names.contains(&"cohort_get")
                && !entities.contains(&"Cohort")
                && !entities.contains(&"Parent"),
            "scalar cohort_id is not entity_ref — do not admit Cohort/Parent: {names:?} {entities:?}"
        );
    }
}
