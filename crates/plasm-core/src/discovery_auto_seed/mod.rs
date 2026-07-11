//! Entity candidate bundles for intent-only auto-seeding (group + diversify discovery evidence).

mod helpers;
mod inject;
mod pool;
mod types;

#[cfg(test)]
mod tests;

pub use pool::diversify_entity_bundles;
pub use types::{
    EntityCandidateBundle, EntityCandidateConfig, EntityCapabilityEvidence,
    DEFAULT_MAX_CAPABILITIES_PER_ENTITY, DEFAULT_MAX_ENTITIES_PER_CATALOG,
    DEFAULT_MAX_ENTITY_CANDIDATES, DEFAULT_RETRIEVE_CAPABILITY_K,
};

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::discovery::{
    CgsCatalog, CgsDiscovery, DiscoveryError, DiscoveryResult, RankedCandidate,
};
use crate::discovery_intent_signals::intent_suggests_workflow_mutation;
use crate::discovery_seed_bundle::intent_requests_cross_catalog_composition;

use helpers::ArcCgs;
use inject::inject_retrieval_targets;
use inject::inject_workflow_mutation_targets;
use pool::{group_candidates_by_entity, merge_required_entity_bundles};

/// Build tenant-scoped entity candidate bundles from lexical discovery (no score thresholds).
pub fn retrieve_entity_candidate_bundles<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<&[String]>,
    config: EntityCandidateConfig,
) -> Result<Vec<types::EntityCandidateBundle>, DiscoveryError>
where
    C: CgsDiscovery + CgsCatalog,
{
    let mut query = capability_query_from_intent_phrase(intent);
    if let Some(ids) = allowed_entry_ids {
        if !ids.is_empty() {
            query.entry_ids = Some(ids.to_vec());
        }
    }
    let discovery = catalog.discover(&query)?;
    let candidates: Vec<RankedCandidate> = discovery
        .candidates
        .iter()
        .take(config.retrieve_k)
        .cloned()
        .collect();

    let catalogs = load_catalog_contexts(
        catalog,
        &candidates,
        &discovery,
        intent,
        allowed_entry_ids,
    )?;

    let mut grouped = group_candidates_by_entity(&candidates, &discovery, &catalogs, config);
    inject_retrieval_targets(&mut grouped, &catalogs, &candidates, intent, &discovery);
    let mut diversified = diversify_entity_bundles(grouped.values().cloned().collect(), config);
    if intent_suggests_workflow_mutation(intent) {
        let mut pool: IndexMap<(String, String), types::EntityCandidateBundle> = diversified
            .iter()
            .map(|b| ((b.entry_id.clone(), b.entity.clone()), b.clone()))
            .collect();
        let keys_before: HashSet<_> = pool.keys().cloned().collect();
        inject_workflow_mutation_targets(&mut pool, &catalogs, intent, &discovery);
        let required: Vec<types::EntityCandidateBundle> = pool
            .iter()
            .filter(|(key, _)| !keys_before.contains(key))
            .map(|(_, bundle)| bundle.clone())
            .collect();
        diversified = merge_required_entity_bundles(diversified, &required, config);
    }
    Ok(diversified)
}

fn load_catalog_contexts<C>(
    catalog: &C,
    candidates: &[RankedCandidate],
    discovery: &DiscoveryResult,
    intent: &str,
    allowed_entry_ids: Option<&[String]>,
) -> Result<IndexMap<String, ArcCgs>, DiscoveryError>
where
    C: CgsCatalog,
{
    let mut catalogs: IndexMap<String, ArcCgs> = IndexMap::new();
    for c in candidates {
        if catalogs.contains_key(&c.entry_id) {
            continue;
        }
        if let Ok(ctx) = catalog.load_context(&c.entry_id) {
            catalogs.insert(c.entry_id.clone(), ctx.cgs);
        }
    }
    if intent_requests_cross_catalog_composition(intent) {
        for entry_id in ["google-sheets", "google-drive"] {
            if catalogs.contains_key(entry_id) {
                continue;
            }
            if allowed_entry_ids.is_some_and(|ids| {
                !ids.is_empty() && !ids.iter().any(|id| id == entry_id)
            }) {
                continue;
            }
            if let Ok(ctx) = catalog.load_context(entry_id) {
                catalogs.insert(entry_id.into(), ctx.cgs);
            }
        }
    }
    for entry_id in discovery.catalog_route.as_slice() {
        if catalogs.contains_key(entry_id) {
            continue;
        }
        if allowed_entry_ids.is_some_and(|ids| {
            !ids.is_empty() && !ids.iter().any(|id| id == entry_id)
        }) {
            continue;
        }
        if let Ok(ctx) = catalog.load_context(entry_id) {
            catalogs.insert(entry_id.clone(), ctx.cgs);
        }
    }
    Ok(catalogs)
}

/// Build intent phrase query (exported for eval harness).
pub fn capability_query_from_intent_phrase(intent: &str) -> crate::discovery::CapabilityQuery {
    crate::discovery::CapabilityQuery {
        phrases: vec![intent.to_string()],
        ..Default::default()
    }
}
