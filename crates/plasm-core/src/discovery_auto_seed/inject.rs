use std::collections::HashSet;

use indexmap::IndexMap;

use crate::discovery::{outgoing_relation_hints_for_entity, DiscoveryResult, RankedCandidate};
use crate::discovery_intent_signals::{
    intent_names_catalog, intent_suggests_github_repo_workflow, intent_suggests_workflow_mutation,
    WORKFLOW_MUTATION_ENTITIES,
};
use crate::discovery_seed_bundle::intent_requests_cross_catalog_composition;
use crate::domain_lexicon;
use crate::schema::{RelationSchema, CGS};

use super::helpers::{candidate_id, capability_id, entity_description_for, ArcCgs};
use super::types::{EntityCandidateBundle, EntityCapabilityEvidence};

fn relation_intent_score(intent_tokens: &HashSet<String>, rel: &RelationSchema) -> u32 {
    let Some(h) = &rel.discovery else {
        return 1;
    };
    if h.qualifier_terms.is_empty() {
        return 1;
    }
    let mut total = 0u32;
    for term in &h.qualifier_terms {
        for tok in domain_lexicon::tokens(term) {
            if intent_tokens.contains(&tok) {
                total = total.saturating_add(1);
            }
        }
    }
    total
}

pub(crate) fn mutation_capabilities_for_entity(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    max: usize,
) -> Vec<EntityCapabilityEvidence> {
    use crate::schema::CapabilityKind;
    let mut out = Vec::new();
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Create,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            out.push(EntityCapabilityEvidence {
                capability_id: capability_id(entry_id, entity, cap.name.as_str()),
                capability_name: cap.name.to_string(),
                kind: format!("{:?}", cap.kind),
                description: cap.description.clone(),
                reason_codes: Vec::new(),
                lexical_score: 1,
            });
            if out.len() >= max {
                return out;
            }
        }
    }
    out
}

fn extra_workflow_mutation_entities(entry_id: &str, intent: &str) -> Vec<&'static str> {
    if entry_id == "github" && intent_suggests_github_repo_workflow(intent) {
        return vec!["Repository", "Branch", "Label"];
    }
    Vec::new()
}

fn inject_relation_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    candidates: &[RankedCandidate],
    intent: &str,
    discovery: &DiscoveryResult,
) {
    let intent_tokens: HashSet<String> = domain_lexicon::tokens(intent).into_iter().collect();
    let present: HashSet<(String, String)> = bundles.keys().cloned().collect();
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut seen_rel_targets: HashSet<(String, String)> = HashSet::new();
    for cand in candidates.iter().take(32) {
        let Some(cgs) = catalogs.get(&cand.entry_id) else {
            continue;
        };
        let Some(ent) = cgs.get_entity(cand.entity.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            if cgs.get_entity(target).is_none() {
                continue;
            }
            if relation_intent_score(&intent_tokens, rel) == 0 {
                continue;
            }
            let key = (cand.entry_id.clone(), target.to_string());
            if present.contains(&key) || !seen_rel_targets.insert(key.clone()) {
                continue;
            }
            bundles.insert(
                key.clone(),
                EntityCandidateBundle {
                    candidate_id: candidate_id(&key.0, &key.1),
                    entry_id: key.0.clone(),
                    entity: key.1.clone(),
                    entity_description: entity_description_for(
                        &discovery.entity_summaries,
                        &key.0,
                        &key.1,
                        Some(cgs.as_ref()),
                    ),
                    max_lexical_score: 0,
                    capabilities: Vec::new(),
                    relation_hints: outgoing_relation_hints_for_entity(
                        cgs.as_ref(),
                        target,
                        crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                    ),
                    catalog_route_evidence: route_set.contains(key.0.as_str()),
                },
            );
        }
    }
}

fn inject_mutation_entity_bundle(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    cgs: &ArcCgs,
    entry_id: &str,
    entity: &str,
    discovery: &DiscoveryResult,
    route_set: &HashSet<&str>,
) {
    let key = (entry_id.to_string(), entity.to_string());
    if bundles.contains_key(&key) {
        return;
    }
    let capabilities = mutation_capabilities_for_entity(cgs.as_ref(), entry_id, entity, 3);
    if capabilities.is_empty() {
        return;
    }
    bundles.insert(
        key.clone(),
        EntityCandidateBundle {
            candidate_id: candidate_id(&key.0, &key.1),
            entry_id: key.0.clone(),
            entity: key.1.clone(),
            entity_description: entity_description_for(
                &discovery.entity_summaries,
                &key.0,
                &key.1,
                Some(cgs.as_ref()),
            ),
            max_lexical_score: 1,
            capabilities,
            relation_hints: outgoing_relation_hints_for_entity(
                cgs.as_ref(),
                entity,
                crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
            ),
            catalog_route_evidence: route_set.contains(entry_id),
        },
    );
}

pub(crate) fn inject_workflow_mutation_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    intent: &str,
    discovery: &DiscoveryResult,
) {
    if !intent_suggests_workflow_mutation(intent) {
        return;
    }
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut catalog_ids: HashSet<String> = bundles
        .keys()
        .map(|(entry_id, _)| entry_id.clone())
        .collect();
    for entry_id in discovery.catalog_route.as_slice() {
        catalog_ids.insert(entry_id.clone());
    }
    for entry_id in catalogs.keys() {
        if intent_names_catalog(entry_id, intent) {
            catalog_ids.insert(entry_id.clone());
        }
    }
    for entry_id in catalog_ids {
        let Some(cgs) = catalogs.get(&entry_id) else {
            continue;
        };
        for entity in WORKFLOW_MUTATION_ENTITIES
            .iter()
            .chain(extra_workflow_mutation_entities(&entry_id, intent).iter())
        {
            if cgs.get_entity(entity).is_none() {
                continue;
            }
            inject_mutation_entity_bundle(bundles, cgs, &entry_id, entity, discovery, &route_set);
        }
    }
}

fn inject_mirror_catalog_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    intent: &str,
    discovery: &DiscoveryResult,
) {
    if !intent_requests_cross_catalog_composition(intent) {
        return;
    }
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    for (catalog, entity) in [("google-sheets", "Spreadsheet"), ("google-drive", "File")] {
        let Some(cgs) = catalogs.get(catalog) else {
            continue;
        };
        if cgs.get_entity(entity).is_none() {
            continue;
        }
        let key = (catalog.to_string(), entity.to_string());
        if bundles.contains_key(&key) {
            continue;
        }
        let mut capabilities = Vec::new();
        for cap in cgs
            .find_capabilities(entity, crate::schema::CapabilityKind::Query)
            .into_iter()
            .chain(cgs.find_capabilities(entity, crate::schema::CapabilityKind::Get))
            .take(2)
        {
            capabilities.push(EntityCapabilityEvidence {
                capability_id: capability_id(catalog, entity, cap.name.as_str()),
                capability_name: cap.name.to_string(),
                kind: format!("{:?}", cap.kind),
                description: cap.description.clone(),
                reason_codes: Vec::new(),
                lexical_score: 1,
            });
        }
        bundles.insert(
            key.clone(),
            EntityCandidateBundle {
                candidate_id: candidate_id(&key.0, &key.1),
                entry_id: key.0.clone(),
                entity: key.1.clone(),
                entity_description: entity_description_for(
                    &discovery.entity_summaries,
                    &key.0,
                    &key.1,
                    Some(cgs.as_ref()),
                ),
                max_lexical_score: 1,
                capabilities,
                relation_hints: outgoing_relation_hints_for_entity(
                    cgs.as_ref(),
                    entity,
                    crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                ),
                catalog_route_evidence: route_set.contains(catalog),
            },
        );
    }
}

pub(crate) fn inject_retrieval_targets(
    grouped: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    candidates: &[RankedCandidate],
    intent: &str,
    discovery: &DiscoveryResult,
) {
    inject_relation_targets(grouped, catalogs, candidates, intent, discovery);
    inject_mirror_catalog_targets(grouped, catalogs, intent, discovery);
}
