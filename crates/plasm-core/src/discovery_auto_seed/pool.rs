use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::discovery::{outgoing_relation_hints_for_entity, DiscoveryResult, RankedCandidate};

use super::helpers::{
    candidate_id, entity_description_for, push_capability_evidence, ArcCgs,
};
use super::types::{EntityCandidateBundle, EntityCandidateConfig};

pub(crate) fn group_candidates_by_entity(
    candidates: &[RankedCandidate],
    discovery: &DiscoveryResult,
    catalogs: &IndexMap<String, ArcCgs>,
    config: EntityCandidateConfig,
) -> IndexMap<(String, String), EntityCandidateBundle> {
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut groups: IndexMap<(String, String), EntityCandidateBundle> = IndexMap::new();

    for cand in candidates {
        let key = (cand.entry_id.clone(), cand.entity.clone());
        let entry = groups.entry(key).or_insert_with(|| {
            let cgs = catalogs.get(&cand.entry_id).map(|a| a.as_ref());
            EntityCandidateBundle {
                candidate_id: candidate_id(&cand.entry_id, &cand.entity),
                entry_id: cand.entry_id.clone(),
                entity: cand.entity.clone(),
                entity_description: entity_description_for(
                    &discovery.entity_summaries,
                    &cand.entry_id,
                    &cand.entity,
                    cgs,
                ),
                max_lexical_score: 0,
                capabilities: Vec::new(),
                relation_hints: cgs
                    .map(|g| {
                        outgoing_relation_hints_for_entity(
                            g,
                            cand.entity.as_str(),
                            crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                        )
                    })
                    .unwrap_or_default(),
                catalog_route_evidence: route_set.contains(cand.entry_id.as_str()),
            }
        });
        entry.max_lexical_score = entry.max_lexical_score.max(cand.score);
        push_capability_evidence(
            entry,
            cand,
            catalogs,
            config.max_capabilities_per_entity,
        );
    }
    groups
}

/// After diversification, reserve slots for post-inject workflow-mutation bundles.
pub(crate) fn merge_required_entity_bundles(
    mut out: Vec<EntityCandidateBundle>,
    required: &[EntityCandidateBundle],
    config: EntityCandidateConfig,
) -> Vec<EntityCandidateBundle> {
    let mut protected: HashSet<String> = HashSet::new();

    for bundle in required {
        if out.iter().any(|b| b.candidate_id == bundle.candidate_id) {
            protected.insert(bundle.candidate_id.clone());
            continue;
        }
        let entry_id = &bundle.entry_id;
        let same_catalog_count = out.iter().filter(|b| &b.entry_id == entry_id).count();
        if same_catalog_count >= config.max_per_catalog {
            if let Some(evict_idx) = out
                .iter()
                .enumerate()
                .filter(|(_, b)| &b.entry_id == entry_id && !protected.contains(&b.candidate_id))
                .min_by_key(|(_, b)| (b.max_lexical_score, b.entity.as_str()))
                .map(|(idx, _)| idx)
            {
                out.remove(evict_idx);
            }
        }
        if out.len() >= config.max_entities {
            if let Some(evict_idx) = out
                .iter()
                .enumerate()
                .filter(|(_, b)| &b.entry_id != entry_id && !protected.contains(&b.candidate_id))
                .min_by_key(|(_, b)| b.max_lexical_score)
                .map(|(idx, _)| idx)
            {
                out.remove(evict_idx);
            } else if let Some(evict_idx) = out
                .iter()
                .enumerate()
                .filter(|(_, b)| &b.entry_id == entry_id && !protected.contains(&b.candidate_id))
                .min_by_key(|(_, b)| b.max_lexical_score)
                .map(|(idx, _)| idx)
            {
                out.remove(evict_idx);
            }
        }
        out.push(bundle.clone());
        protected.insert(bundle.candidate_id.clone());
    }
    out
}

/// Diversify entity bundles: best per catalog first, then fill by score with per-catalog cap.
pub fn diversify_entity_bundles(
    mut bundles: Vec<EntityCandidateBundle>,
    config: EntityCandidateConfig,
) -> Vec<EntityCandidateBundle> {
    bundles.sort_by(|a, b| {
        b.max_lexical_score
            .cmp(&a.max_lexical_score)
            .then_with(|| a.entry_id.cmp(&b.entry_id))
            .then_with(|| a.entity.cmp(&b.entity))
    });

    let mut out: Vec<EntityCandidateBundle> = Vec::new();
    let mut per_catalog: HashMap<String, usize> = HashMap::new();
    let mut seen_catalog: HashSet<String> = HashSet::new();

    for b in &bundles {
        if out.len() >= config.max_entities {
            break;
        }
        if seen_catalog.insert(b.entry_id.clone()) {
            *per_catalog.entry(b.entry_id.clone()).or_insert(0) += 1;
            out.push(b.clone());
        }
    }

    for b in &bundles {
        if out.len() >= config.max_entities {
            break;
        }
        if out.iter().any(|x| x.candidate_id == b.candidate_id) {
            continue;
        }
        let count = per_catalog.entry(b.entry_id.clone()).or_insert(0);
        if *count >= config.max_per_catalog {
            continue;
        }
        *count += 1;
        out.push(b.clone());
    }
    out
}
