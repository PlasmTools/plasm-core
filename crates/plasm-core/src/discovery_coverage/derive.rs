//! Derive a coverage plan from intent + catalog metadata (v2 slots).
//!
//! Deterministic layer = catalog phrase search + CGS graph only. Plain text →
//! controlled vocabulary is an LLM job (closed `w#` witnesses / multipass narrow).

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery_controlled_lexicon::explicit_named_catalogs_from_intent;
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_seed_catalog::{build_catalog_seed_index, match_intent_to_catalog};
use crate::schema::{CapabilityKind, CGS};

use super::types::{DiscoveryCoveragePlan, ProviderConstraint, RequirementSlot};

/// Absolute BM25 milli floor for committing an entity_hint.
const ROOT_HINT_MIN_SCORE: i32 = 1;

/// Relative margin: top must beat next distinct entity by this fraction of top (or absolute 1).
const ROOT_HINT_MARGIN_NUM: i32 = 1;
const ROOT_HINT_MARGIN_DEN: i32 = 5;

/// Derive deterministic coverage slots from intent text.
pub fn derive_coverage_plan(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    catalog_route: &[String],
) -> DiscoveryCoveragePlan {
    let named = explicit_named_catalogs_from_intent(
        catalogs,
        intent,
        if allowed_entry_ids.is_empty() {
            None
        } else {
            Some(allowed_entry_ids)
        },
    );
    let provider_constraint = provider_constraint_from(named.clone());

    let mut slots = Vec::new();

    // Federation from ≥2 explicitly named catalogs — not English composition verbs.
    if named.len() >= 2 {
        for entry_id in &named {
            slots.push(RequirementSlot::FederateSlot {
                entry_id: entry_id.clone(),
            });
        }
    }

    let entity_hits = collect_entity_hits(intent, catalogs, allowed_entry_ids);
    let root_hint = select_root_entity_hint(&entity_hits, &provider_constraint);
    let mutate_op = infer_mutation_kind_from_index(intent, catalogs, allowed_entry_ids);
    let relation_hops = infer_relation_hops(catalogs, allowed_entry_ids, &entity_hits, &root_hint);

    match mutate_op {
        Some(op) => slots.push(RequirementSlot::MutateAnchor {
            op,
            entity_hint: root_hint.clone(),
        }),
        None => slots.push(RequirementSlot::ReadRoot {
            entity_hint: root_hint.clone(),
        }),
    }

    for hop in relation_hops {
        if !slots.contains(&hop) {
            slots.push(hop);
        }
    }

    DiscoveryCoveragePlan {
        slots,
        provider_constraint,
        catalog_route: catalog_route.to_vec(),
    }
}

fn provider_constraint_from(named: Vec<String>) -> ProviderConstraint {
    if named.is_empty() {
        ProviderConstraint::Unbranded
    } else {
        ProviderConstraint::Locked(named)
    }
}

/// Mutate kind from BM25 capability scores (operation/target text is in the index docs).
fn infer_mutation_kind_from_index(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> Option<CapabilityKind> {
    let search = CatalogSearchIndex::build_from_index_map(catalogs);
    let mut best_score = 0i32;
    let mut best_kind = None;
    for (entry_id, cgs) in catalogs {
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        let index = build_catalog_seed_index(entry_id, cgs);
        if let Some((kind, score)) = index.best_mutation_kind_for_intent(&search, intent) {
            if score > best_score {
                best_score = score;
                best_kind = Some(kind);
            }
        }
    }
    best_kind
}

/// Entity BM25 hits for derive, confidence, and LLM slot glossaries.
pub fn collect_entity_hits(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> Vec<(i32, String, String)> {
    let intent_class = DiscoveryIntentClass::ReadListNav;
    let search = CatalogSearchIndex::build_from_index_map(catalogs);
    let mut hits: Vec<(i32, String, String)> = Vec::new();
    for (entry_id, cgs) in catalogs {
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        let index = build_catalog_seed_index(entry_id, cgs);
        let workflow = match_intent_to_catalog(intent, &index, &search, &intent_class);
        for entity in &workflow.matched_entities {
            if cgs
                .entities
                .get(entity.as_str())
                .is_some_and(|e| e.abstract_entity)
            {
                continue;
            }
            let score = index
                .entity_phrase_match_score(&search, entity, intent)
                .max(1);
            hits.push((score, entry_id.clone(), entity.clone()));
        }
    }
    hits.sort_by_key(|right| std::cmp::Reverse(right.0));
    hits
}

fn select_root_entity_hint(
    hits: &[(i32, String, String)],
    provider_constraint: &ProviderConstraint,
) -> Option<String> {
    if hits.is_empty() {
        return None;
    }

    let scoped: Vec<&(i32, String, String)> = match provider_constraint {
        ProviderConstraint::Locked(ids) => hits
            .iter()
            .filter(|(_, entry_id, _)| ids.iter().any(|id| id == entry_id))
            .collect(),
        _ => hits.iter().collect(),
    };
    // Locked with no in-catalog hits → no root (do not leak competitor homologues).
    if matches!(provider_constraint, ProviderConstraint::Locked(_)) && scoped.is_empty() {
        return None;
    }

    let mut ranked: Vec<(i32, String)> = scoped
        .iter()
        .map(|(score, _, entity)| (*score, entity.clone()))
        .collect();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let (top_score, top_entity) = ranked.first()?.clone();
    if top_score < ROOT_HINT_MIN_SCORE {
        return None;
    }

    if matches!(provider_constraint, ProviderConstraint::Locked(_)) {
        return Some(top_entity);
    }

    let next_distinct = ranked
        .iter()
        .skip(1)
        .find(|(_, entity)| !entity.eq_ignore_ascii_case(&top_entity));
    if let Some((next_score, _)) = next_distinct {
        let margin = (top_score * ROOT_HINT_MARGIN_NUM / ROOT_HINT_MARGIN_DEN).max(1);
        if top_score.saturating_sub(*next_score) < margin {
            return None;
        }
    }

    Some(top_entity)
}

fn infer_relation_hops(
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    entity_hits: &[(i32, String, String)],
    root_hint: &Option<String>,
) -> Vec<RequirementSlot> {
    let Some(root) = root_hint else {
        return Vec::new();
    };

    let mut by_catalog: std::collections::HashMap<String, Vec<(i32, String)>> =
        std::collections::HashMap::new();
    for (score, entry_id, entity) in entity_hits {
        if *score <= 0 || entity == root {
            continue;
        }
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        by_catalog
            .entry(entry_id.clone())
            .or_default()
            .push((*score, entity.clone()));
    }

    let mut out = Vec::new();
    for (entry_id, mut entities) in by_catalog {
        entities.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        entities.dedup_by(|a, b| a.1 == b.1);
        let Some((_, leaf)) = entities.first() else {
            continue;
        };
        let Some(cgs) = catalogs.get(&entry_id) else {
            continue;
        };
        if let Some((wire, target)) = find_direct_relation(cgs, root, leaf) {
            out.push(RequirementSlot::RelationHop { wire, target });
        }
    }

    let mut by_target: std::collections::BTreeMap<String, RequirementSlot> =
        std::collections::BTreeMap::new();
    for hop in out {
        if let RequirementSlot::RelationHop { ref target, .. } = hop {
            by_target.entry(target.clone()).or_insert(hop);
        }
    }
    by_target.into_values().collect()
}

fn find_direct_relation(cgs: &CGS, from: &str, to: &str) -> Option<(String, String)> {
    let entity = cgs.entities.get(from)?;
    for (wire, rel) in &entity.relations {
        if rel.target_resource.as_str() == to {
            return Some((wire.to_string(), to.to_string()));
        }
    }
    None
}
