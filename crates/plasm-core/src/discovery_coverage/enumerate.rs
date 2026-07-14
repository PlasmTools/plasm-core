//! Coverage-driven entity enumeration and SeedPlan construction.
//!
//! Retrieve is **BM25 catalog search + 1-hop CGS expand**, not a federated schema dump.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery::{outgoing_relation_hints_for_entity, DISCOVERY_OUTGOING_RELATIONS_MAX};
use crate::discovery_auto_seed::{
    diversify_entity_bundles, EntityCandidateBundle, EntityCandidateConfig,
    EntityCapabilityEvidence,
};
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_intent_signals::is_auxiliary_entity_for_mutation;
use crate::discovery_seed_catalog::{build_catalog_seed_index, match_intent_to_catalog};
use crate::schema::{CapabilityKind, CapabilitySchema, CGS};

use super::types::{
    DiscoveryCoveragePlan, ProviderConstraint, RequirementSlot, SeedPlan, SeedSatisfiability,
};

const READ_KINDS: [CapabilityKind; 3] = [
    CapabilityKind::Query,
    CapabilityKind::Search,
    CapabilityKind::Get,
];

const MUTATE_KINDS: [CapabilityKind; 4] = [
    CapabilityKind::Create,
    CapabilityKind::Update,
    CapabilityKind::Delete,
    CapabilityKind::Action,
];

/// Enumerate entity bundles from **BM25 hits + 1-hop graph expand**.
///
/// Does **not** admit every entity that merely has Query/Get capabilities. Empty BM25
/// evidence ⇒ empty pool for that catalog (fail-closed hard_miss upstream).
pub fn enumerate_schema_bundles(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    catalog_route: &[String],
) -> Vec<EntityCandidateBundle> {
    let route_set: HashSet<&str> = catalog_route.iter().map(String::as_str).collect();
    // Prefer mutation-family admission so capability operation/target terms fire.
    let intent_class = DiscoveryIntentClass::default();
    let search = CatalogSearchIndex::build_from_index_map(catalogs);
    let mut bundles = Vec::new();

    for (entry_id, cgs) in catalogs {
        if !allowed_entry_ids.is_empty() && !allowed_entry_ids.iter().any(|id| id == entry_id) {
            continue;
        }
        let index = build_catalog_seed_index(entry_id, cgs);
        let workflow = match_intent_to_catalog(intent, &index, &search, &intent_class);
        if workflow.matched_entities.is_empty() {
            continue;
        }

        let seeds = workflow.matched_entities.clone();
        let expanded = expand_one_hop(cgs, &seeds);

        for entity_name in &expanded {
            let Some(entity_def) = cgs.get_entity(entity_name.as_str()) else {
                continue;
            };
            let phrase_score = index.entity_phrase_match_score(&search, entity_name, intent) as u32;
            let op_hit = workflow.matched_operation_entities.contains(entity_name);
            let mut_hit = workflow.matched_mutation_entities.contains(entity_name);
            let seed_hit = seeds.contains(entity_name);
            let max_lexical_score = phrase_score
                .max(if op_hit { 2 } else { 0 })
                .max(if mut_hit { 1 } else { 0 })
                .max(if seed_hit { 1 } else { 0 })
                // Graph neighbor admitted only via expand — keep below true phrase hits.
                .max(if !seed_hit { 1 } else { 0 });

            let caps = collect_entity_capabilities(entry_id, cgs, entity_name, max_lexical_score);
            if caps.is_empty() {
                continue;
            }

            bundles.push(EntityCandidateBundle {
                candidate_id: format!("{entry_id}:{entity_name}"),
                entry_id: entry_id.clone(),
                entity: entity_name.clone(),
                entity_description: entity_def.description.clone(),
                max_lexical_score,
                capabilities: caps,
                relation_hints: outgoing_relation_hints_for_entity(
                    cgs,
                    entity_name.as_str(),
                    DISCOVERY_OUTGOING_RELATIONS_MAX,
                ),
                catalog_route_evidence: route_set.contains(entry_id.as_str()),
            });
        }
    }

    bundles.sort_by(|left, right| {
        right
            .max_lexical_score
            .cmp(&left.max_lexical_score)
            .then_with(|| left.entry_id.cmp(&right.entry_id))
            .then_with(|| left.entity.cmp(&right.entity))
    });
    diversify_entity_bundles(bundles, EntityCandidateConfig::default())
}

/// 1-hop undirected CGS expand from BM25 seed entities.
fn expand_one_hop(cgs: &CGS, seeds: &HashSet<String>) -> HashSet<String> {
    let mut out = seeds.clone();
    for entity in seeds {
        if let Some(ent) = cgs.entities.get(entity.as_str()) {
            for rel in ent.relations.values() {
                out.insert(rel.target_resource.to_string());
            }
        }
        for (parent_name, parent) in &cgs.entities {
            if parent
                .relations
                .values()
                .any(|rel| rel.target_resource.as_str() == entity.as_str())
            {
                out.insert(parent_name.to_string());
            }
        }
    }
    out
}

fn collect_entity_capabilities(
    entry_id: &str,
    cgs: &CGS,
    entity: &str,
    entity_score: u32,
) -> Vec<EntityCapabilityEvidence> {
    let mut caps = Vec::new();
    for kind in READ_KINDS.into_iter().chain(MUTATE_KINDS.into_iter()) {
        for cap in cgs.find_capabilities(entity, kind) {
            caps.push(capability_evidence(
                entry_id,
                entity,
                cap,
                entity_score.max(1),
            ));
        }
    }
    caps
}

fn capability_evidence(
    entry_id: &str,
    entity: &str,
    cap: &CapabilitySchema,
    lexical_score: u32,
) -> EntityCapabilityEvidence {
    EntityCapabilityEvidence {
        capability_id: format!("{entry_id}:{entity}:{}", cap.name),
        capability_name: cap.name.to_string(),
        kind: format!("{:?}", cap.kind),
        description: cap.description.clone(),
        reason_codes: Vec::new(),
        lexical_score,
    }
}

/// Score per-seed slot coverage.
pub fn score_satisfiability(
    plan: &DiscoveryCoveragePlan,
    bundles: &[EntityCandidateBundle],
    catalogs: &IndexMap<String, CGS>,
) -> Vec<SeedSatisfiability> {
    let slot_count = plan.slots.len();
    if slot_count == 0 {
        return bundles
            .iter()
            .map(|bundle| seed_row(bundle, Vec::new(), Vec::new()))
            .collect();
    }

    let graph_parents = build_graph_parents(catalogs);

    bundles
        .iter()
        .filter(|bundle| provider_allowed(&plan.provider_constraint, &bundle.entry_id))
        .filter_map(|bundle| {
            let cgs = catalogs.get(&bundle.entry_id)?;
            let mut direct = Vec::new();
            let mut via_relation = Vec::new();
            for (idx, slot) in plan.slots.iter().enumerate() {
                if slot_satisfied_direct(bundle, cgs, slot) {
                    direct.push(idx);
                } else if slot_satisfied_via_relation(bundle, cgs, slot, &graph_parents) {
                    via_relation.push(idx);
                }
            }
            if direct.is_empty() && via_relation.is_empty() {
                return None;
            }
            Some(seed_row(bundle, direct, via_relation))
        })
        .collect()
}

/// Build per-provider SeedPlans (1–3 seeds) that jointly cover all slots.
pub fn enumerate_seed_plans(
    plan: &DiscoveryCoveragePlan,
    seeds: &[SeedSatisfiability],
    catalogs: &IndexMap<String, CGS>,
) -> BTreeMapPlans {
    let _slot_count = plan.slots.len();
    let signature = plan.slot_signature();
    let needs_relation_pair = plan
        .slots
        .iter()
        .any(|slot| matches!(slot, RequirementSlot::RelationHop { .. }));
    let federation_slots: Vec<_> = plan
        .slots
        .iter()
        .enumerate()
        .filter_map(|(idx, slot)| match slot {
            RequirementSlot::FederateSlot { entry_id } => Some((idx, entry_id.clone())),
            _ => None,
        })
        .collect();

    let mut by_provider: BTreeMapPlans = BTreeMapPlans::new();

    // Group seeds by provider.
    let mut seeds_by_provider: HashMap<String, Vec<&SeedSatisfiability>> = HashMap::new();
    for seed in seeds {
        seeds_by_provider
            .entry(seed.entry_id.clone())
            .or_default()
            .push(seed);
    }

    if !federation_slots.is_empty() {
        if let Some(tuple) = federation_plan(plan, &seeds_by_provider, &signature) {
            let provider = tuple.primary_provider().unwrap_or("federation").to_string();
            by_provider.entry(provider).or_default().push(tuple);
        }
    }

    for (provider, provider_seeds) in &seeds_by_provider {
        let mut plans = Vec::new();

        // Single-seed plans that cover all non-federation slots.
        let required = primary_slot_indices(plan);
        let required_all = if required.is_empty() {
            all_slot_indices(plan)
        } else {
            required.clone()
        };
        for seed in provider_seeds {
            if seed_covers_all_primary(seed, plan) {
                if is_auxiliary_entity_for_mutation(&seed.entity)
                    && provider_seeds.iter().any(|other| {
                        other.candidate_id != seed.candidate_id
                            && seed_covers_all_primary(other, plan)
                            && !is_auxiliary_entity_for_mutation(&other.entity)
                    })
                {
                    continue;
                }
                if let Some(seed_plan) = SeedPlan::from_seeds(vec![(*seed).clone()], &required_all)
                {
                    plans.push(seed_plan.with_signature(signature.clone()));
                }
            } else if seed_covers_required_read_root(seed, plan)
                && plan
                    .slots
                    .iter()
                    .all(|slot| !matches!(slot, RequirementSlot::RelationHop { .. }))
            {
                // Single-entity read when no relation hop slots were derived.
                if let Some(seed_plan) = SeedPlan::from_seeds(vec![(*seed).clone()], &required_all)
                {
                    plans.push(seed_plan.with_signature(signature.clone()));
                }
            }
        }

        // Two-seed plans for relation hops / multi-entity.
        if needs_relation_pair || required_all.len() > 1 {
            let ranked: Vec<_> = {
                let mut ranked = provider_seeds.clone();
                ranked.sort_by(|left, right| {
                    right
                        .lexical_score
                        .cmp(&left.lexical_score)
                        .then_with(|| left.entity.cmp(&right.entity))
                });
                ranked.into_iter().take(8).collect::<Vec<_>>()
            };
            for i in 0..ranked.len() {
                for j in (i + 1)..ranked.len() {
                    let pair = vec![ranked[i].clone(), ranked[j].clone()];
                    if !pair_covers_all_primary(&pair, plan) {
                        continue;
                    }
                    if !relation_compatible(&pair, catalogs) && needs_relation_pair {
                        continue;
                    }
                    if let Some(seed_plan) = SeedPlan::from_seeds(pair, &required_all) {
                        plans.push(seed_plan.with_signature(signature.clone()));
                    }
                }
            }
        }

        // Prefer non-auxiliary / parent roots when scores tie.
        plans.sort_by(|left, right| {
            left.seeds
                .len()
                .cmp(&right.seeds.len())
                .then_with(|| right.lexical_score.cmp(&left.lexical_score))
                .then_with(|| {
                    let left_aux = left
                        .seeds
                        .iter()
                        .any(|s| is_auxiliary_entity_for_mutation(&s.entity));
                    let right_aux = right
                        .seeds
                        .iter()
                        .any(|s| is_auxiliary_entity_for_mutation(&s.entity));
                    left_aux.cmp(&right_aux)
                })
                .then_with(|| {
                    left.seeds
                        .first()
                        .map(|s| s.entity.as_str())
                        .unwrap_or("")
                        .cmp(right.seeds.first().map(|s| s.entity.as_str()).unwrap_or(""))
                })
        });
        plans.dedup_by(|a, b| a.candidate_ids() == b.candidate_ids());
        if !plans.is_empty() {
            by_provider.insert(provider.clone(), plans);
        }
    }

    by_provider
}

type BTreeMapPlans = std::collections::BTreeMap<String, Vec<SeedPlan>>;

fn primary_slot_indices(plan: &DiscoveryCoveragePlan) -> Vec<usize> {
    plan.slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| !matches!(slot, RequirementSlot::FederateSlot { .. }))
        .map(|(idx, _)| idx)
        .collect()
}

fn all_slot_indices(plan: &DiscoveryCoveragePlan) -> Vec<usize> {
    (0..plan.slots.len()).collect()
}

fn seed_covers_all_primary(seed: &SeedSatisfiability, plan: &DiscoveryCoveragePlan) -> bool {
    plan.slots
        .iter()
        .enumerate()
        .all(|(idx, slot)| matches!(slot, RequirementSlot::FederateSlot { .. }) || seed.covers(idx))
}

fn seed_covers_required_read_root(seed: &SeedSatisfiability, plan: &DiscoveryCoveragePlan) -> bool {
    plan.slots.iter().enumerate().all(|(idx, slot)| match slot {
        RequirementSlot::ReadRoot { .. } | RequirementSlot::FederateSlot { .. } => seed.covers(idx),
        RequirementSlot::MutateAnchor { .. } => seed.covers(idx),
        RequirementSlot::RelationHop { .. } => true,
    })
}

fn pair_covers_all_primary(seeds: &[SeedSatisfiability], plan: &DiscoveryCoveragePlan) -> bool {
    plan.slots.iter().enumerate().all(|(idx, slot)| {
        matches!(slot, RequirementSlot::FederateSlot { .. })
            || seeds.iter().any(|seed| seed.covers(idx))
    })
}

fn relation_compatible(seeds: &[SeedSatisfiability], catalogs: &IndexMap<String, CGS>) -> bool {
    if seeds.len() != 2 {
        return true;
    }
    let left = &seeds[0];
    let right = &seeds[1];
    if left.entry_id != right.entry_id {
        return false;
    }
    let Some(cgs) = catalogs.get(&left.entry_id) else {
        return false;
    };
    relation_reaches_entities(cgs, &left.entity, &right.entity)
        || relation_reaches_entities(cgs, &right.entity, &left.entity)
}

fn relation_reaches_entities(cgs: &CGS, from: &str, to: &str) -> bool {
    cgs.entities.get(from).is_some_and(|entity| {
        entity
            .relations
            .values()
            .any(|rel| rel.target_resource.as_str() == to)
    })
}

fn federation_plan(
    plan: &DiscoveryCoveragePlan,
    seeds_by_provider: &HashMap<String, Vec<&SeedSatisfiability>>,
    signature: &str,
) -> Option<SeedPlan> {
    let catalogs: Vec<String> = plan
        .slots
        .iter()
        .filter_map(|slot| match slot {
            RequirementSlot::FederateSlot { entry_id } => Some(entry_id.clone()),
            _ => None,
        })
        .collect();
    if catalogs.is_empty() {
        return None;
    }
    let mut seeds = Vec::new();
    for catalog in &catalogs {
        let best = seeds_by_provider
            .get(catalog)?
            .iter()
            .max_by_key(|seed| (seed.lexical_score, seed.entity.as_str()))?;
        seeds.push((*best).clone());
    }
    SeedPlan::from_seeds(seeds, &all_slot_indices(plan))
        .map(|p| p.with_signature(signature.to_string()))
}

fn build_graph_parents(catalogs: &IndexMap<String, CGS>) -> HashMap<(String, String), Vec<String>> {
    let mut graph_parents: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (entry_id, cgs) in catalogs {
        for (entity_name, entity) in &cgs.entities {
            for rel in entity.relations.values() {
                graph_parents
                    .entry((entry_id.clone(), rel.target_resource.to_string()))
                    .or_default()
                    .push(entity_name.to_string());
            }
        }
    }
    graph_parents
}

fn seed_row(
    bundle: &EntityCandidateBundle,
    direct: Vec<usize>,
    via_relation: Vec<usize>,
) -> SeedSatisfiability {
    SeedSatisfiability {
        entry_id: bundle.entry_id.clone(),
        entity: bundle.entity.clone(),
        candidate_id: bundle.candidate_id.clone(),
        lexical_score: bundle.max_lexical_score,
        catalog_route_evidence: bundle.catalog_route_evidence,
        direct_slots: direct,
        via_relation_slots: via_relation,
        bundle: bundle.clone(),
    }
}

fn provider_allowed(constraint: &ProviderConstraint, entry_id: &str) -> bool {
    match constraint {
        ProviderConstraint::Unbranded => true,
        ProviderConstraint::Locked(locked) => locked.iter().any(|id| id == entry_id),
        ProviderConstraint::Rejected(rejected) => !rejected.iter().any(|id| id == entry_id),
    }
}

fn slot_satisfied_direct(
    bundle: &EntityCandidateBundle,
    cgs: &CGS,
    slot: &RequirementSlot,
) -> bool {
    match slot {
        RequirementSlot::ReadRoot { entity_hint } => {
            if let Some(hint) = entity_hint {
                if !entity_matches_hint(&bundle.entity, hint) {
                    return false;
                }
            }
            bundle_has_read_kind(bundle)
        }
        RequirementSlot::RelationHop { target, .. } => {
            bundle.entity == *target || relation_reaches(bundle, cgs, target)
        }
        RequirementSlot::MutateAnchor { op, entity_hint } => {
            if let Some(target) = entity_hint {
                if !entity_matches_hint(&bundle.entity, target) {
                    return false;
                }
            }
            bundle_has_kind(bundle, *op) || bundle_has_mutation_kind(bundle)
        }
        RequirementSlot::FederateSlot { entry_id } => bundle.entry_id == *entry_id,
    }
}

fn slot_satisfied_via_relation(
    bundle: &EntityCandidateBundle,
    cgs: &CGS,
    slot: &RequirementSlot,
    graph_parents: &HashMap<(String, String), Vec<String>>,
) -> bool {
    match slot {
        RequirementSlot::ReadRoot { entity_hint } => {
            let Some(hint) = entity_hint else {
                return bundle_has_read_kind(bundle);
            };
            if entity_matches_hint(&bundle.entity, hint) {
                return bundle_has_read_kind(bundle);
            }
            relation_reaches(bundle, cgs, hint) && bundle_has_read_kind(bundle)
        }
        RequirementSlot::RelationHop { wire, target } => {
            bundle
                .relation_hints
                .split(';')
                .any(|hint| hint.trim() == format!("{wire}→{target}"))
                || relation_reaches(bundle, cgs, target)
        }
        RequirementSlot::MutateAnchor { op, entity_hint } => {
            let target = entity_hint.as_deref().unwrap_or(&bundle.entity);
            if bundle.entity == target {
                return bundle_has_kind(bundle, *op) || bundle_has_mutation_kind(bundle);
            }
            let parents = graph_parents
                .get(&(bundle.entry_id.clone(), target.to_string()))
                .cloned()
                .unwrap_or_default();
            parents.contains(&bundle.entity)
                && (bundle_has_kind(bundle, *op) || bundle_has_mutation_kind(bundle))
        }
        RequirementSlot::FederateSlot { entry_id } => bundle.entry_id == *entry_id,
    }
}

fn bundle_has_read_kind(bundle: &EntityCandidateBundle) -> bool {
    bundle
        .capabilities
        .iter()
        .any(|cap| matches!(cap.kind.as_str(), "Query" | "Search" | "Get"))
}

fn bundle_has_kind(bundle: &EntityCandidateBundle, kind: CapabilityKind) -> bool {
    let label = format!("{kind:?}");
    bundle.capabilities.iter().any(|cap| cap.kind == label)
}

fn bundle_has_mutation_kind(bundle: &EntityCandidateBundle) -> bool {
    bundle
        .capabilities
        .iter()
        .any(|cap| matches!(cap.kind.as_str(), "Create" | "Update" | "Delete" | "Action"))
}

fn entity_matches_hint(entity: &str, hint: &str) -> bool {
    entity.eq_ignore_ascii_case(hint)
        || entity
            .to_ascii_lowercase()
            .contains(&hint.to_ascii_lowercase())
}

fn relation_reaches(bundle: &EntityCandidateBundle, cgs: &CGS, target: &str) -> bool {
    let Some(entity) = cgs.entities.get(bundle.entity.as_str()) else {
        return false;
    };
    entity
        .relations
        .values()
        .any(|rel| rel.target_resource.as_str() == target)
        || bundle
            .relation_hints
            .split(';')
            .any(|hint| hint.trim().ends_with(&format!("→{target}")))
}

/// Build Arc-wrapped catalogs map for graph operations.
#[allow(dead_code)]
pub fn arc_catalogs(catalogs: &IndexMap<String, CGS>) -> IndexMap<String, Arc<CGS>> {
    catalogs
        .iter()
        .map(|(id, cgs)| (id.clone(), Arc::new(cgs.clone())))
        .collect()
}
