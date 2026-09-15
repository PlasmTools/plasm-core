//! Exact teaching exposure for selected business and prerequisite capabilities.

use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
    ExposureSurfaceDelta,
};
use crate::{CapabilityKind, FieldType, CGS};
use std::collections::BTreeSet;

/// When any Get / Query / Search is selected for an entity, admit the rest of that
/// read family on the same row type. Identity Get and fuzzy Search are compositional
/// — selecting one must not silently drop the other.
fn close_read_family_capability_ids(cgs: &CGS, selected: &[String]) -> Vec<String> {
    let mut out: Vec<String> = selected.to_vec();
    let mut seen: BTreeSet<String> = selected.iter().cloned().collect();
    let mut domains: BTreeSet<String> = BTreeSet::new();
    for name in selected {
        let Some(cap) = cgs.capabilities.get(name.as_str()) else {
            continue;
        };
        if matches!(
            cap.kind,
            CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
        ) {
            domains.insert(cap.domain.to_string());
        }
    }
    admit_read_family_for_domains(cgs, domains, &mut out, &mut seen);
    out
}

fn admit_read_family_for_domains(
    cgs: &CGS,
    domains: impl IntoIterator<Item = String>,
    out: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) {
    for domain in domains {
        if let Some(cap) = cgs.primary_get_capability(domain.as_str()) {
            if seen.insert(cap.name.to_string()) {
                out.push(cap.name.to_string());
            }
        }
        for kind in [CapabilityKind::Query, CapabilityKind::Search] {
            for cap in cgs.find_capabilities(domain.as_str(), kind) {
                if seen.insert(cap.name.to_string()) {
                    out.push(cap.name.to_string());
                }
            }
        }
    }
}

/// Foreign identity holes on taught Query/Search ParentScope or mutator args.
///
/// A hole is identity-typed only when its named value is `entity_ref` to another
/// entity. Integer / string scalars that merely share a name with an `id_field`
/// (or a "primary key" gloss) are not identity — that is catalog authoring, not
/// exposure inference. BackendSelection credentials and Get arguments are
/// skipped so token-identity Gets stay off this close.
fn identity_scope_target_domains(cgs: &CGS, selected: &[String]) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for name in selected {
        let Some(cap) = cgs.capabilities.get(name.as_str()) else {
            continue;
        };
        let own = cap.domain.as_str();
        match cap.kind {
            CapabilityKind::Query | CapabilityKind::Search => {
                collect_entity_ref_targets(cgs, own, cap.scope_params().iter(), &mut targets);
            }
            CapabilityKind::Create
            | CapabilityKind::Update
            | CapabilityKind::Delete
            | CapabilityKind::Action => {
                collect_entity_ref_targets(cgs, own, cap.invocation_object_fields(), &mut targets);
            }
            CapabilityKind::Get => {}
        }
    }
    targets
}

fn collect_entity_ref_targets<'a, I>(
    cgs: &CGS,
    own_domain: &str,
    fields: I,
    targets: &mut BTreeSet<String>,
) where
    I: IntoIterator<Item = &'a crate::schema::InputFieldSchema>,
{
    for field in fields {
        let Ok(nv) = field.named_value(cgs) else {
            continue;
        };
        let FieldType::EntityRef { target, .. } = &nv.field_type else {
            continue;
        };
        if target.as_str() != own_domain {
            targets.insert(target.to_string());
        }
    }
}

/// Seeded entities — domains of the caller-selected capability IDs — teach every
/// authored create / update / delete / action. Identity-scope close must not
/// use this path; it only admits the target's read family.
fn close_seeded_entity_mutator_capability_ids(cgs: &CGS, selected: &[String]) -> Vec<String> {
    let mut out: Vec<String> = selected.to_vec();
    let mut seen: BTreeSet<String> = selected.iter().cloned().collect();
    let mut domains: BTreeSet<String> = BTreeSet::new();
    for name in selected {
        let Some(cap) = cgs.capabilities.get(name.as_str()) else {
            continue;
        };
        domains.insert(cap.domain.to_string());
    }
    for domain in domains {
        for kind in [
            CapabilityKind::Create,
            CapabilityKind::Update,
            CapabilityKind::Delete,
            CapabilityKind::Action,
        ] {
            for cap in cgs.find_capabilities(domain.as_str(), kind) {
                if seen.insert(cap.name.to_string()) {
                    out.push(cap.name.to_string());
                }
            }
        }
    }
    out
}

/// When a taught Query/Search scope or mutator arg is `entity_ref` to another
/// entity, admit that entity's Query/Get/Search so the hole is bindable from a
/// rowset. One hop from the already-selected set — not a 2-hop neighbourhood,
/// and not Friend-style admission of an unrelated peer.
fn close_identity_scope_target_capability_ids(cgs: &CGS, selected: &[String]) -> Vec<String> {
    let mut out: Vec<String> = selected.to_vec();
    let mut seen: BTreeSet<String> = selected.iter().cloned().collect();
    admit_read_family_for_domains(
        cgs,
        identity_scope_target_domains(cgs, selected),
        &mut out,
        &mut seen,
    );
    out
}

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

/// Project selected IDs without intent scoring or implicit parent promotion.
/// Seeded domains (owners of those IDs) teach every authored mutator — a
/// selected Query must not silently drop an authored Update on the same entity.
/// Read-family siblings (Get identity ↔ Query/Search list) are closed so an
/// intent-selected Search cannot unteach an authored Get on the same entity.
/// Identity-scope targets of taught Query/Search / mutator `entity_ref` holes
/// receive the same read-family close so the hole has a lawful rowset fill;
/// that close does not admit the target's mutators.
pub fn selected_capability_surface(
    cgs: &CGS,
    entry_id: &str,
    capabilities: &[String],
) -> Result<ExposureSurfaceDelta, String> {
    let capabilities = close_seeded_entity_mutator_capability_ids(cgs, capabilities);
    let capabilities = close_read_family_capability_ids(cgs, &capabilities);
    let capabilities = close_identity_scope_target_capability_ids(cgs, &capabilities);
    let mut surface = ExposureSurface::default();
    for name in &capabilities {
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
    fn search_only_selection_admits_sibling_get() {
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
            names.contains(&"securednote_get"),
            "Get identity must be admitted with Search: {names:?}"
        );
        assert!(
            !names.contains(&"authsession_login"),
            "read-family close must not pull unrelated mutators: {names:?}"
        );
    }

    #[test]
    fn get_only_selection_admits_sibling_search() {
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
            names.contains(&"securednote_search"),
            "Search must be admitted with Get: {names:?}"
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
    fn query_entity_ref_scope_admits_target_read_family() {
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
        assert!(
            names.contains(&"child_settle"),
            "seeded Child must teach every authored mutator: {names:?}"
        );
        assert!(
            names.contains(&"parent_query") && names.contains(&"parent_get"),
            "Parent Query/Get must be first-wave so parent_id is bindable: {names:?}"
        );
        assert!(
            entities.contains(&"Parent") && entities.contains(&"Child"),
            "Parent entity must be exposed: {entities:?}"
        );
        assert!(
            !names.contains(&"parent_create"),
            "identity-scope close must not admit Parent mutators: {names:?}"
        );
        assert!(
            !names.contains(&"friend_query") && !entities.contains(&"Friend"),
            "unrelated Friend must stay untaught (no prefer_from_parent, no entity_ref hole): {names:?} {entities:?}"
        );
        assert!(
            !names.contains(&"member_query")
                && !names.contains(&"cohort_get")
                && !entities.contains(&"Cohort"),
            "integer-named scope must not infer an identity target: {names:?} {entities:?}"
        );
    }

    #[test]
    fn query_selection_admits_seeded_entity_mutators_not_identity_scope_writes() {
        let cgs = load_schema_dir(&scoped_query_matrix_dir()).expect("scoped_query_matrix");
        let delta =
            selected_capability_surface(&cgs, "scoped_query_matrix", &["child_query".into()])
                .expect("child query");
        let names = capability_names(&delta);
        assert!(
            names.contains(&"child_settle"),
            "first wave must teach authored Child mutator child_settle: {names:?}"
        );
        assert!(
            !names.contains(&"parent_create"),
            "identity-scope Parent must not gain writes from Child seed: {names:?}"
        );
    }

    #[test]
    fn mutator_entity_ref_arg_admits_target_read_family() {
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
            names.contains(&"parent_query") && names.contains(&"parent_get"),
            "mutator entity_ref arg must admit Parent read family: {names:?}"
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
