//! Merge upstream LLM slot hints and ground entity hints against intent evidence.

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery_seed_catalog::build_catalog_seed_index;
use crate::schema::{CapabilityKind, CGS};

use super::types::{DiscoveryCoveragePlan, ProviderConstraint, RequirementSlot};

/// Apply LLM-extracted slots without overriding brand locks or federation.
pub fn merge_llm_slots(
    heuristic: &DiscoveryCoveragePlan,
    llm_slots: Vec<RequirementSlot>,
) -> DiscoveryCoveragePlan {
    let mut slots = Vec::new();

    for slot in &heuristic.slots {
        if matches!(slot, RequirementSlot::FederateSlot { .. }) {
            slots.push(slot.clone());
        }
    }

    for slot in llm_slots {
        if matches!(slot, RequirementSlot::FederateSlot { .. }) {
            continue;
        }
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }

    // If LLM returned nothing useful beyond federation, keep heuristic non-federation slots.
    if slots
        .iter()
        .all(|s| matches!(s, RequirementSlot::FederateSlot { .. }))
    {
        for slot in &heuristic.slots {
            if !matches!(slot, RequirementSlot::FederateSlot { .. }) && !slots.contains(slot) {
                slots.push(slot.clone());
            }
        }
    }

    DiscoveryCoveragePlan {
        slots,
        provider_constraint: heuristic.provider_constraint.clone(),
        catalog_route: heuristic.catalog_route.clone(),
    }
}

/// Sanitize LLM slots against loaded CGS entity names, then require intent grounding.
pub fn sanitize_llm_slots(
    llm_slots: Vec<RequirementSlot>,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    provider_constraint: &ProviderConstraint,
) -> Vec<RequirementSlot> {
    let mut entity_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (entry_id, cgs) in catalogs {
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        for name in cgs.entities.keys() {
            entity_names.insert(name.to_string());
        }
    }

    let membership: Vec<RequirementSlot> = llm_slots
        .into_iter()
        .filter_map(|slot| match slot {
            RequirementSlot::ReadRoot {
                entity_hint: Some(hint),
            } if !entity_names.contains(&hint) => {
                Some(RequirementSlot::ReadRoot { entity_hint: None })
            }
            RequirementSlot::MutateAnchor {
                op,
                entity_hint: Some(hint),
            } if !entity_names.contains(&hint) => Some(RequirementSlot::MutateAnchor {
                op,
                entity_hint: None,
            }),
            RequirementSlot::RelationHop { target, .. } if !entity_names.contains(&target) => None,
            other => Some(other),
        })
        .collect();

    let stub = DiscoveryCoveragePlan {
        slots: membership,
        provider_constraint: provider_constraint.clone(),
        catalog_route: Vec::new(),
    };
    ground_slots(&stub, intent, catalogs, allowed_entry_ids).slots
}

/// Drop unsupported entity hints. Membership in CGS is not enough — the intent must
/// support the hint lexically, or an exclusive brand lock must own that entity.
pub fn ground_slots(
    plan: &DiscoveryCoveragePlan,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> DiscoveryCoveragePlan {
    let brand_locked: Option<&[String]> = match &plan.provider_constraint {
        ProviderConstraint::Locked(ids) if ids.len() == 1 => Some(ids.as_slice()),
        _ => None,
    };

    let grounded_roots: Vec<String> = plan
        .slots
        .iter()
        .filter_map(|slot| match slot {
            RequirementSlot::ReadRoot {
                entity_hint: Some(hint),
            }
            | RequirementSlot::MutateAnchor {
                entity_hint: Some(hint),
                ..
            } if hint_is_grounded(hint, intent, catalogs, allowed_entry_ids, brand_locked) => {
                Some(hint.clone())
            }
            _ => None,
        })
        .collect();

    let mut slots = Vec::new();
    for slot in &plan.slots {
        match slot {
            RequirementSlot::ReadRoot {
                entity_hint: Some(hint),
            } => {
                if hint_is_grounded(hint, intent, catalogs, allowed_entry_ids, brand_locked) {
                    slots.push(slot.clone());
                } else {
                    slots.push(RequirementSlot::ReadRoot { entity_hint: None });
                }
            }
            RequirementSlot::MutateAnchor {
                op,
                entity_hint: Some(hint),
            } => {
                if hint_is_grounded(hint, intent, catalogs, allowed_entry_ids, brand_locked) {
                    slots.push(slot.clone());
                } else {
                    slots.push(RequirementSlot::MutateAnchor {
                        op: *op,
                        entity_hint: None,
                    });
                }
            }
            RequirementSlot::RelationHop { wire, target } => {
                if hop_is_grounded(
                    wire,
                    target,
                    intent,
                    catalogs,
                    allowed_entry_ids,
                    &grounded_roots,
                ) {
                    slots.push(slot.clone());
                }
            }
            other => slots.push(other.clone()),
        }
    }

    DiscoveryCoveragePlan {
        slots,
        provider_constraint: plan.provider_constraint.clone(),
        catalog_route: plan.catalog_route.clone(),
    }
}

fn hint_is_grounded(
    entity: &str,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    brand_locked: Option<&[String]>,
) -> bool {
    if entity_is_abstract(entity, catalogs, allowed_entry_ids) {
        return brand_lock_owns_entity(entity, catalogs, brand_locked);
    }
    if brand_lock_owns_entity(entity, catalogs, brand_locked) {
        return true;
    }
    entity_lexical_support(entity, intent, catalogs, allowed_entry_ids) > 0
}

fn entity_is_abstract(
    entity: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> bool {
    catalogs.iter().any(|(entry_id, cgs)| {
        (allowed_entry_ids.is_empty() || allowed_entry_ids.iter().any(|id| id == entry_id))
            && cgs
                .entities
                .get(entity)
                .is_some_and(|def| def.abstract_entity)
    })
}

fn brand_lock_owns_entity(
    entity: &str,
    catalogs: &IndexMap<String, CGS>,
    brand_locked: Option<&[String]>,
) -> bool {
    let Some(locked) = brand_locked else {
        return false;
    };
    locked.iter().any(|entry_id| {
        catalogs
            .get(entry_id)
            .is_some_and(|cgs| cgs.entities.contains_key(entity))
    })
}

fn entity_lexical_support(
    entity: &str,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> i32 {
    let search = CatalogSearchIndex::build_from_index_map(catalogs);
    let mut best = 0i32;
    for (entry_id, cgs) in catalogs {
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        if !cgs.entities.contains_key(entity) {
            continue;
        }
        let index = build_catalog_seed_index(entry_id, cgs);
        best = best.max(index.entity_phrase_match_score(&search, entity, intent));
    }
    best
}

fn hop_is_grounded(
    wire: &str,
    target: &str,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    grounded_roots: &[String],
) -> bool {
    if entity_lexical_support(target, intent, catalogs, allowed_entry_ids) > 0 {
        return true;
    }
    for root in grounded_roots {
        for (entry_id, cgs) in catalogs {
            if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
                continue;
            }
            let Some(entity) = cgs.entities.get(root.as_str()) else {
                continue;
            };
            for (rel_name, rel) in &entity.relations {
                let wire_ok = wire.is_empty() || rel_name.as_str() == wire;
                if wire_ok && rel.target_resource.as_str() == target {
                    return true;
                }
            }
        }
    }
    false
}

/// Parse BAML op label into CapabilityKind (defaults to Action).
pub fn capability_kind_from_label(label: &str) -> CapabilityKind {
    match label.to_ascii_lowercase().as_str() {
        "create" => CapabilityKind::Create,
        "update" => CapabilityKind::Update,
        "delete" => CapabilityKind::Delete,
        "query" | "search" | "get" => CapabilityKind::Query,
        _ => CapabilityKind::Action,
    }
}
