//! Entity candidate bundles for intent-only auto-seeding (group + diversify discovery evidence).

mod helpers;
mod inject;
mod pool;
mod types;

#[cfg(test)]
mod tests;

pub use pool::diversify_entity_bundles;
pub use types::{
    EntityCandidateBundle, EntityCandidateConfig, EntityCandidateRetrieveResult,
    EntityCapabilityEvidence, DEFAULT_MAX_CAPABILITIES_PER_ENTITY,
    DEFAULT_MAX_ENTITIES_PER_CATALOG, DEFAULT_MAX_ENTITY_CANDIDATES, DEFAULT_RETRIEVE_CAPABILITY_K,
};

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::discovery::{
    CgsCatalog, CgsDiscovery, DiscoveryError, DiscoveryResult, RankedCandidate,
};
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_seed_catalog::CatalogWorkflowContext;

use helpers::ArcCgs;
use inject::inject_retrieval_targets;
use inject::inject_workflow_mutation_targets;
use pool::{
    group_candidates_by_entity, merge_required_entity_bundles, readmit_scored_entity_drops,
};

/// Build tenant-scoped entity candidate bundles from lexical discovery (no score thresholds).
pub fn retrieve_entity_candidate_bundles<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<&[String]>,
    config: EntityCandidateConfig,
    intent_class: &DiscoveryIntentClass,
    named_catalogs: &[String],
) -> Result<EntityCandidateRetrieveResult, DiscoveryError>
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
        named_catalogs,
        allowed_entry_ids,
    )?;
    let catalog_context =
        build_catalog_workflow_context(catalog, &catalogs, intent, intent_class, named_catalogs);

    let mut grouped = group_candidates_by_entity(&candidates, &discovery, &catalogs, config);
    inject_retrieval_targets(
        &mut grouped,
        &catalogs,
        &candidates,
        intent,
        &discovery,
        named_catalogs,
    );
    let pre_diversify: Vec<types::EntityCandidateBundle> = grouped.values().cloned().collect();
    let mut diversified = diversify_entity_bundles(pre_diversify.clone(), config);
    let graph =
        crate::discovery_candidate_graph::TypedCandidateGraph::build(&pre_diversify, &catalogs);
    diversified = graph.closure_aware_merge(diversified, &[]);
    if inject::workflow_inject_active_for(intent_class, &catalog_context) {
        let mut pool: IndexMap<(String, String), types::EntityCandidateBundle> = diversified
            .iter()
            .map(|b| ((b.entry_id.clone(), b.entity.clone()), b.clone()))
            .collect();
        let keys_before: HashSet<_> = pool.keys().cloned().collect();
        inject_workflow_mutation_targets(
            &mut pool,
            &catalogs,
            intent,
            &discovery,
            &catalog_context,
            named_catalogs,
        );
        for bundle in &mut diversified {
            let key = (bundle.entry_id.clone(), bundle.entity.clone());
            if let Some(updated) = pool.get(&key) {
                let refresh = catalog_context.suggests_multi_entity_workflow(&key.0)
                    && (!updated.capabilities.is_empty()
                        && (bundle.capabilities.is_empty()
                            || updated.capabilities.iter().any(|cap| cap.kind == "Create")
                                && !bundle.capabilities.iter().any(|cap| cap.kind == "Create")));
                if refresh || (bundle.capabilities.is_empty() && !updated.capabilities.is_empty()) {
                    *bundle = updated.clone();
                }
            }
        }
        let mut required: Vec<types::EntityCandidateBundle> = pool
            .iter()
            .filter(|(key, _)| !keys_before.contains(key))
            .map(|(_, bundle)| bundle.clone())
            .collect();
        for entry_id in catalog_context.branded_entry_ids() {
            if !catalog_context.suggests_repo_scoped_workflow(&entry_id) {
                continue;
            }
            if let Some(root) = catalog_context.workflow_root_entity(&entry_id) {
                let key = (entry_id.clone(), root.clone());
                if let Some(bundle) = pool.get(&key) {
                    if !required
                        .iter()
                        .any(|b| b.entry_id == entry_id && b.entity == root)
                    {
                        required.push(bundle.clone());
                    }
                }
            }
        }
        diversified = merge_required_entity_bundles(diversified, &required, config);
        for entry_id in catalog_context.branded_entry_ids() {
            if catalog_context.suggests_repo_scoped_workflow(&entry_id) {
                if let Some(root) = catalog_context.workflow_root_entity(&entry_id) {
                    let key = (entry_id.clone(), root);
                    if let Some(bundle) = pool.get(&key) {
                        if !diversified
                            .iter()
                            .any(|b| b.entry_id == entry_id && b.entity == bundle.entity)
                        {
                            diversified = merge_required_entity_bundles(
                                diversified,
                                std::slice::from_ref(bundle),
                                config,
                            );
                        }
                    }
                }
            }
        }
    }
    // Workflow inject / merge_required can evict scored golds — restore once more from inject-time group.
    diversified = readmit_scored_entity_drops(diversified, &pre_diversify, config);
    let candidate_graph =
        crate::discovery_candidate_graph::TypedCandidateGraph::build(&diversified, &catalogs);
    Ok(EntityCandidateRetrieveResult {
        bundles: diversified,
        catalog_context,
        intent_class: intent_class.clone(),
        named_catalogs: named_catalogs.to_vec(),
        candidate_graph,
    })
}

fn build_catalog_workflow_context<C>(
    catalog: &C,
    catalogs: &IndexMap<String, ArcCgs>,
    intent: &str,
    intent_class: &DiscoveryIntentClass,
    named_catalogs: &[String],
) -> CatalogWorkflowContext
where
    C: CgsDiscovery,
{
    let map: std::collections::HashMap<String, &crate::schema::CGS> = catalogs
        .iter()
        .map(|(entry_id, cgs)| (entry_id.clone(), cgs.as_ref()))
        .collect();
    CatalogWorkflowContext::build_with_search(
        &map,
        intent,
        intent_class,
        named_catalogs,
        catalog.search_index(),
    )
}

fn load_catalog_contexts<C>(
    catalog: &C,
    candidates: &[RankedCandidate],
    discovery: &DiscoveryResult,
    _intent: &str,
    named_catalogs: &[String],
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
    if named_catalogs.len() >= 2 {
        let mut load_candidates: Vec<String> = discovery.catalog_route.as_slice().to_vec();
        if let Some(ids) = allowed_entry_ids {
            for entry_id in ids {
                if !load_candidates.iter().any(|id| id == entry_id) {
                    load_candidates.push(entry_id.clone());
                }
            }
        }
        for entry_id in load_candidates {
            if catalogs.contains_key(&entry_id) {
                continue;
            }
            if !named_catalogs
                .iter()
                .any(|catalog| catalog.eq_ignore_ascii_case(&entry_id))
            {
                continue;
            }
            if let Ok(ctx) = catalog.load_context(&entry_id) {
                catalogs.insert(entry_id, ctx.cgs);
            }
        }
    }
    for entry_id in discovery.catalog_route.as_slice() {
        if catalogs.contains_key(entry_id) {
            continue;
        }
        if allowed_entry_ids
            .is_some_and(|ids| !ids.is_empty() && !ids.iter().any(|id| id == entry_id))
        {
            continue;
        }
        if let Ok(ctx) = catalog.load_context(entry_id) {
            catalogs.insert(entry_id.clone(), ctx.cgs);
        }
    }
    if let Some(ids) = allowed_entry_ids {
        for entry_id in ids {
            if catalogs.contains_key(entry_id) {
                continue;
            }
            if named_catalogs
                .iter()
                .any(|catalog| catalog.eq_ignore_ascii_case(entry_id))
            {
                if let Ok(ctx) = catalog.load_context(entry_id) {
                    catalogs.insert(entry_id.clone(), ctx.cgs);
                }
            }
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
