//! Bridge coverage retrieve to lexical Graph-RAG auto-seed pools.

use crate::discovery::{CgsCatalog, CgsDiscovery, DiscoveryError};
use crate::discovery_auto_seed::{EntityCandidateConfig, EntityCandidateRetrieveResult};
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_seed_pipeline::load_catalogs_for_entries;

use super::evaluate::{evaluate_coverage, evaluate_plan};
use super::route::route_coverage;
use super::select::select_minimal_seed_set;
use super::types::CoverageRoute;

/// Retrieve candidate bundles for closed `s#` narrow.
///
/// **MCP / Graph-RAG path:** lexical `discover` + diversify + workflow inject
/// ([`retrieve_entity_candidate_bundles`](crate::discovery_auto_seed::retrieve_entity_candidate_bundles)).
/// Coverage enumeration must not dump the federated schema into the LLM pool.
pub fn retrieve_via_coverage<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<&[String]>,
    intent_class: &DiscoveryIntentClass,
    named_catalogs: &[String],
) -> Result<EntityCandidateRetrieveResult, DiscoveryError>
where
    C: CgsDiscovery + CgsCatalog,
{
    crate::discovery_auto_seed::retrieve_entity_candidate_bundles(
        catalog,
        intent,
        allowed_entry_ids,
        EntityCandidateConfig::default(),
        intent_class,
        named_catalogs,
    )
}

fn discover_catalog_route<C: CgsDiscovery>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: &[String],
) -> Vec<String> {
    let mut query = crate::discovery::CapabilityQuery {
        phrases: vec![intent.to_string()],
        ..Default::default()
    };
    if !allowed_entry_ids.is_empty() {
        query.entry_ids = Some(allowed_entry_ids.to_vec());
    }
    catalog
        .discover(&query)
        .ok()
        .map(|result| result.catalog_route.as_slice().to_vec())
        .unwrap_or_default()
}

/// Coverage-first selection without LLM when route is clarify/hard_miss/unique select.
pub fn coverage_route_selection<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: &[String],
) -> Result<(super::types::CoveragePipelineResult, CoverageRoute), DiscoveryError>
where
    C: CgsDiscovery + CgsCatalog,
{
    let allowed: Vec<String> = if allowed_entry_ids.is_empty() {
        catalog
            .list_entries()
            .into_iter()
            .map(|meta| meta.entry_id)
            .collect()
    } else {
        allowed_entry_ids.to_vec()
    };
    let catalogs = load_catalogs_for_entries(catalog, &allowed);
    let catalog_route = discover_catalog_route(catalog, intent, &allowed);
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &catalog_route);
    let route = route_coverage(&evaluation);
    let minimized = select_minimal_seed_set(&evaluation, &route);
    let mut selection = super::route::route_to_selection_raw(&route, &evaluation);
    if matches!(route, CoverageRoute::Select { .. }) && !minimized.is_empty() {
        let (provider, plan, tie_candidates) = match &route {
            CoverageRoute::Select {
                provider,
                plan,
                tie_candidates,
                ..
            } => (provider.clone(), plan.clone(), tie_candidates.clone()),
            _ => unreachable!(),
        };
        selection = super::route::route_to_selection_raw(
            &CoverageRoute::Select {
                selected: minimized,
                provider,
                tie_candidates,
                plan,
            },
            &evaluation,
        );
    }
    let pipeline = super::types::CoveragePipelineResult {
        evaluation,
        route: route.clone(),
        selection: Some(selection),
    };
    Ok((pipeline, route))
}

/// Like [`coverage_route_selection`] but with an explicit abstain margin (holdout sweep).
pub fn coverage_route_selection_with_margin<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: &[String],
    margin: u32,
) -> Result<(super::types::CoveragePipelineResult, CoverageRoute), DiscoveryError>
where
    C: CgsDiscovery + CgsCatalog,
{
    let allowed: Vec<String> = if allowed_entry_ids.is_empty() {
        catalog
            .list_entries()
            .into_iter()
            .map(|meta| meta.entry_id)
            .collect()
    } else {
        allowed_entry_ids.to_vec()
    };
    let catalogs = load_catalogs_for_entries(catalog, &allowed);
    let catalog_route = discover_catalog_route(catalog, intent, &allowed);
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &catalog_route);
    let route = super::route::route_coverage_with_margin(&evaluation, margin);
    let minimized = select_minimal_seed_set(&evaluation, &route);
    let mut selection = super::route::route_to_selection_raw(&route, &evaluation);
    if matches!(route, CoverageRoute::Select { .. }) && !minimized.is_empty() {
        let (provider, plan, tie_candidates) = match &route {
            CoverageRoute::Select {
                provider,
                plan,
                tie_candidates,
                ..
            } => (provider.clone(), plan.clone(), tie_candidates.clone()),
            _ => unreachable!(),
        };
        selection = super::route::route_to_selection_raw(
            &CoverageRoute::Select {
                selected: minimized,
                provider,
                tie_candidates,
                plan,
            },
            &evaluation,
        );
    }
    let pipeline = super::types::CoveragePipelineResult {
        evaluation,
        route: route.clone(),
        selection: Some(selection),
    };
    Ok((pipeline, route))
}

/// Run evaluate → route → select for a pre-built plan (e.g. after LLM slot merge).
pub fn coverage_pipeline_for_plan<C>(
    catalog: &C,
    intent: &str,
    plan: super::types::DiscoveryCoveragePlan,
    allowed_entry_ids: &[String],
) -> Result<super::types::CoveragePipelineResult, DiscoveryError>
where
    C: CgsDiscovery + CgsCatalog,
{
    let allowed: Vec<String> = if allowed_entry_ids.is_empty() {
        catalog
            .list_entries()
            .into_iter()
            .map(|meta| meta.entry_id)
            .collect()
    } else {
        allowed_entry_ids.to_vec()
    };
    let catalogs = load_catalogs_for_entries(catalog, &allowed);
    let catalog_route = discover_catalog_route(catalog, intent, &allowed);
    let evaluation = evaluate_plan(
        &plan,
        intent,
        &catalogs,
        &allowed,
        &catalog_route,
    );
    Ok(super::build_coverage_pipeline(evaluation))
}

#[allow(dead_code)]
pub fn legacy_entity_candidate_config() -> EntityCandidateConfig {
    EntityCandidateConfig::default()
}
