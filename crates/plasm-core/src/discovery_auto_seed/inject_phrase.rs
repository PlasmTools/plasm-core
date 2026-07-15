//! Phrase-named leaf inject for teaching-satellite pool membership.
//!
//! Uses catalog-authored `discovery.names` + stamp satellite shape
//! ([`CatalogWorkflowContext::entity_is_satellite_shape`]).

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::discovery::{outgoing_relation_hints_for_entity, DiscoveryResult};
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::schema::{CapabilityKind, DiscoverySeedClass, DiscoverySeedNav, CGS};

use super::helpers::{candidate_id, entity_description_for, ArcCgs};
use super::inject::{
    mutation_capabilities_for_entity_with_intent, push_capability_evidence,
    read_capabilities_for_entity,
};
use super::types::{EntityCandidateBundle, EntityCapabilityEvidence};

/// Kept in lock-step with [`crate::discovery_seed_witness::MAX_TEACHING_SATELLITES`].
const MAX_PHRASE_LEAVES_PER_CATALOG: usize = 4;

/// Upsert brand-locked satellite-shaped entities whose authored discovery names hit
/// the intent. Returns the reserved leaf rows (new + boosted) for a single pool merge.
pub(crate) fn inject_phrase_named_leaves(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    intent: &str,
    discovery: &DiscoveryResult,
    catalog_context: &CatalogWorkflowContext,
) -> Vec<EntityCandidateBundle> {
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut catalog_ids: HashSet<String> =
        catalog_context.branded_entry_ids().into_iter().collect();
    for entry_id in discovery.catalog_route.as_slice() {
        catalog_ids.insert(entry_id.clone());
    }
    let mut reserved: IndexMap<(String, String), EntityCandidateBundle> = IndexMap::new();
    for entry_id in catalog_ids {
        let Some(cgs) = catalogs.get(&entry_id) else {
            continue;
        };
        let pool_entities: HashSet<String> = bundles
            .keys()
            .filter(|(e, _)| e == &entry_id)
            .map(|(_, ent)| ent.clone())
            .collect();
        let mut scored: Vec<(i32, String)> = Vec::new();
        for entity_name in cgs.entities.keys() {
            let entity = entity_name.as_str();
            if !catalog_context.entity_is_authored_satellite_leaf(
                &entry_id,
                entity,
                &pool_entities,
                Some(cgs.as_ref()),
            ) {
                continue;
            }
            let phrase = catalog_context.entity_phrase_match_score(&entry_id, entity, intent);
            let priority = phrase.max(1);
            let attach_boost = pool_entities.iter().any(|parent| {
                catalog_context
                    .relation_seed_nav(&entry_id, parent, entity)
                    .is_some_and(|nav| matches!(nav, DiscoverySeedNav::Attach))
            });
            let priority = if attach_boost {
                priority + 1_000
            } else if catalog_context
                .entity_seed_class(&entry_id, entity)
                .is_some_and(|c| matches!(c, DiscoverySeedClass::Dependent))
            {
                priority + 500
            } else {
                priority
            };
            scored.push((priority, entity.to_string()));
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.truncate(MAX_PHRASE_LEAVES_PER_CATALOG);
        for (priority, entity) in scored {
            let key = (entry_id.clone(), entity.clone());
            let leaf_score = (priority as u32).clamp(1, 64);
            if let Some(existing) = bundles.get_mut(&key) {
                existing.max_lexical_score = existing.max_lexical_score.max(leaf_score);
                reserved.insert(key, existing.clone());
                continue;
            }
            let mut caps = mutation_capabilities_for_entity_with_intent(
                cgs.as_ref(),
                &entry_id,
                &entity,
                3,
                intent,
                Some(catalog_context),
            );
            if caps.is_empty() {
                caps = read_capabilities_for_entity(cgs.as_ref(), &entry_id, &entity, 3);
            }
            if caps.is_empty() {
                caps = any_capabilities_for_entity(cgs.as_ref(), &entry_id, &entity, 3);
            }
            if caps.is_empty() {
                continue;
            }
            let bundle = EntityCandidateBundle {
                candidate_id: candidate_id(&key.0, &key.1),
                entry_id: key.0.clone(),
                entity: key.1.clone(),
                entity_description: entity_description_for(
                    &discovery.entity_summaries,
                    &key.0,
                    &key.1,
                    Some(cgs.as_ref()),
                ),
                max_lexical_score: leaf_score,
                capabilities: caps,
                relation_hints: outgoing_relation_hints_for_entity(
                    cgs.as_ref(),
                    &entity,
                    crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                ),
                catalog_route_evidence: route_set.contains(entry_id.as_str()),
            };
            bundles.insert(key.clone(), bundle.clone());
            reserved.insert(key, bundle);
        }
    }
    reserved.into_values().collect()
}

fn any_capabilities_for_entity(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    max: usize,
) -> Vec<EntityCapabilityEvidence> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for kind in [
        CapabilityKind::Create,
        CapabilityKind::Query,
        CapabilityKind::Get,
        CapabilityKind::Update,
        CapabilityKind::Delete,
        CapabilityKind::Action,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if push_capability_evidence(&mut out, &mut seen, entry_id, entity, cap, max) {
                return out;
            }
        }
    }
    out
}
