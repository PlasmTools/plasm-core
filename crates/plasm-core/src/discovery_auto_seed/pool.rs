use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::discovery::{outgoing_relation_hints_for_entity, DiscoveryResult, RankedCandidate};

use super::helpers::{candidate_id, entity_description_for, push_capability_evidence, ArcCgs};
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
        push_capability_evidence(entry, cand, catalogs, config.max_capabilities_per_entity);
    }
    groups
}

/// After diversification, force `required` into the pool (evicting unprotected rows).
/// When a required id is already present, refresh score/caps from the reserved row.
pub(crate) fn merge_required_entity_bundles(
    mut out: Vec<EntityCandidateBundle>,
    required: &[EntityCandidateBundle],
    config: EntityCandidateConfig,
) -> Vec<EntityCandidateBundle> {
    let mut protected: HashSet<String> = HashSet::new();

    for bundle in required {
        if let Some(existing) = out
            .iter_mut()
            .find(|b| b.candidate_id == bundle.candidate_id)
        {
            existing.max_lexical_score = existing.max_lexical_score.max(bundle.max_lexical_score);
            if existing.capabilities.is_empty() && !bundle.capabilities.is_empty() {
                existing.capabilities = bundle.capabilities.clone();
            }
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
///
/// Afterwards, re-admit discover-scored entities that lost the cut (see
/// [`readmit_scored_entity_drops`]) so ready-case golds survive sibling noise without raising
/// the global schema dump size.
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
    readmit_scored_entity_drops(out, &bundles, config)
}

/// After diversify, re-admit discover-scored entity bundles dropped by per-catalog / global caps.
///
/// Injected `max_lexical_score == 0` neighbors stay subject to the hard cut; only bundles with a
/// positive discover/group score are reserved so ready-case golds survive sibling PR/Thread noise
/// without restoring a schema dump.
pub(crate) fn readmit_scored_entity_drops(
    diversified: Vec<EntityCandidateBundle>,
    pre_diversify: &[EntityCandidateBundle],
    config: EntityCandidateConfig,
) -> Vec<EntityCandidateBundle> {
    /// Extra per-catalog slots beyond [`EntityCandidateConfig::max_per_catalog`] for scored survivors.
    const EXTRA_SCORED_PER_CATALOG: usize = 4;

    let in_pool: HashSet<String> = diversified.iter().map(|b| b.candidate_id.clone()).collect();
    let mut drops: Vec<EntityCandidateBundle> = pre_diversify
        .iter()
        .filter(|b| b.max_lexical_score > 0 && !in_pool.contains(&b.candidate_id))
        .cloned()
        .collect();
    drops.sort_by(|a, b| {
        b.max_lexical_score
            .cmp(&a.max_lexical_score)
            .then_with(|| a.entry_id.cmp(&b.entry_id))
            .then_with(|| a.entity.cmp(&b.entity))
    });

    let mut out = diversified;
    let mut per_catalog: HashMap<String, usize> = HashMap::new();
    for b in &out {
        *per_catalog.entry(b.entry_id.clone()).or_insert(0) += 1;
    }
    let catalog_cap = config
        .max_per_catalog
        .saturating_add(EXTRA_SCORED_PER_CATALOG);

    for bundle in drops {
        if out.iter().any(|b| b.candidate_id == bundle.candidate_id) {
            continue;
        }
        let count = per_catalog.get(&bundle.entry_id).copied().unwrap_or(0);
        if count >= catalog_cap {
            continue;
        }
        if out.len() >= config.max_entities {
            let evict_idx = out
                .iter()
                .enumerate()
                .filter(|(_, b)| b.max_lexical_score == 0)
                .min_by_key(|(_, b)| b.entity.as_str())
                .map(|(idx, _)| idx)
                .or_else(|| {
                    out.iter()
                        .enumerate()
                        .filter(|(_, b)| b.max_lexical_score < bundle.max_lexical_score)
                        .min_by_key(|(_, b)| (b.max_lexical_score, b.entity.as_str()))
                        .map(|(idx, _)| idx)
                });
            let Some(idx) = evict_idx else {
                continue;
            };
            let removed = out.remove(idx);
            if let Some(c) = per_catalog.get_mut(&removed.entry_id) {
                *c = c.saturating_sub(1);
            }
        }
        *per_catalog.entry(bundle.entry_id.clone()).or_insert(0) += 1;
        out.push(bundle);
    }
    out
}

/// Lift score-0 inject neighbors that hit authored entity phrases / workflow matches so
/// [`readmit_scored_entity_drops`] and witness corpus can keep teaching-satellite leaves.
pub(crate) fn boost_zero_score_phrase_hit_leaves(
    bundles: &mut [EntityCandidateBundle],
    catalog_context: &crate::discovery_seed_catalog::CatalogWorkflowContext,
    intent: &str,
) {
    for bundle in bundles.iter_mut() {
        if bundle.max_lexical_score > 0 {
            continue;
        }
        let phrase =
            catalog_context.entity_phrase_match_score(&bundle.entry_id, &bundle.entity, intent);
        if phrase > 0 {
            bundle.max_lexical_score = phrase.max(1) as u32;
            continue;
        }
        if catalog_context
            .workflow_match(&bundle.entry_id)
            .is_some_and(|m| m.matched_entities.contains(&bundle.entity))
        {
            bundle.max_lexical_score = 1;
        }
    }
}
