//! Unified discovery coverage pipeline (v2: SeedPlan).
//!
//! Slot derive uses catalog-authored phrase search + CGS graph only — not English NLP.

mod confidence;
mod derive;
mod enumerate;
mod evaluate;
mod merge_slots;
mod postselect;
mod retrieve;
mod reasoning;
mod route;
mod select;
mod types;

#[cfg(test)]
mod tests;

pub use confidence::{
    collect_derive_entity_hits, derive_confidence, DeriveConfidence, LowConfidenceReason,
};
pub use derive::{collect_entity_hits, derive_coverage_plan};
pub use enumerate::{enumerate_schema_bundles, enumerate_seed_plans, score_satisfiability};
pub use evaluate::{
    coverage_entity_recall, coverage_plan_recall, evaluate_coverage, evaluate_plan,
    gold_in_satisfiable,
};
pub use merge_slots::{
    capability_kind_from_label, ground_slots, merge_llm_slots, sanitize_llm_slots,
};
pub use postselect::postprocess_coverage_selection;
pub use retrieve::{
    coverage_pipeline_for_plan, coverage_route_selection, coverage_route_selection_with_margin,
    retrieve_via_coverage,
};
pub use reasoning::format_coverage_reasoning;
pub use route::{
    can_ready, can_ready_with_margin, route_coverage, route_coverage_with_margin,
    route_to_selection_raw,
};
pub use select::{resolve_seeds_from_ids, select_best_plan, select_minimal_seed_set, selection_matches_gold};
pub use types::{
    CoverageEvaluation, CoveragePipelineResult, CoverageRoute, CoverageShadowMetrics,
    DiscoveryCoveragePlan, DiscoveryRequirement, ProviderAmbiguity, ProviderConstraint,
    RequirementSlot, SeedPlan, SeedSatisfiability, READY_MARGIN,
};

use indexmap::IndexMap;

use crate::discovery::{CapabilityQuery, CgsDiscovery};
use crate::discovery_seed_select::SeedSelectionRaw;
use crate::schema::CGS;

/// Run the full coverage pipeline: derive → enumerate → evaluate → route → select.
pub fn run_coverage_pipeline(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> CoveragePipelineResult {
    let catalog_route = infer_catalog_route(allowed_entry_ids);
    let evaluation = evaluate_coverage(intent, catalogs, allowed_entry_ids, &catalog_route);
    finalize_pipeline(evaluation)
}

/// Coverage pipeline with live catalog route from discover().
pub fn run_coverage_pipeline_with_discover<C>(
    catalog: &C,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> CoveragePipelineResult
where
    C: CgsDiscovery,
{
    let catalog_route = discover_catalog_route(catalog, intent, allowed_entry_ids);
    let evaluation = evaluate_coverage(intent, catalogs, allowed_entry_ids, &catalog_route);
    finalize_pipeline(evaluation)
}

fn finalize_pipeline(evaluation: CoverageEvaluation) -> CoveragePipelineResult {
    let route = route_coverage(&evaluation);
    let minimized = select_minimal_seed_set(&evaluation, &route);
    let mut selection = route_to_selection_raw(&route, &evaluation);
    if let CoverageRoute::Select {
        provider,
        plan,
        tie_candidates,
        ..
    } = &route
    {
        if !minimized.is_empty() {
            let mut ready = route_to_selection_raw(
                &CoverageRoute::Select {
                    selected: minimized,
                    provider: provider.clone(),
                    tie_candidates: tie_candidates.clone(),
                    plan: plan.clone(),
                },
                &evaluation,
            );
            std::mem::swap(&mut selection, &mut ready);
        }
    }
    CoveragePipelineResult {
        evaluation,
        route,
        selection: Some(selection),
    }
}

/// Build shadow metrics for eval harness.
pub fn coverage_shadow_metrics(
    pipeline: &CoveragePipelineResult,
    acceptable_sets: &[Vec<(String, String)>],
    final_selection: Option<&SeedSelectionRaw>,
) -> CoverageShadowMetrics {
    let coverage_ambiguous = matches!(
        pipeline.evaluation.ambiguity,
        ProviderAmbiguity::Between { .. }
    ) || matches!(pipeline.route, CoverageRoute::Clarify { .. });
    let coverage_satisfiable = !pipeline.evaluation.satisfiable_plans_by_provider.is_empty()
        || !pipeline.evaluation.satisfiable_federation_tuples.is_empty();
    let coverage_plan_recall_v = coverage_plan_recall(&pipeline.evaluation, acceptable_sets);
    let coverage_entity_recall_v = coverage_entity_recall(&pipeline.evaluation, acceptable_sets);
    let coverage_gold_recall = coverage_plan_recall_v;
    let plan_route_decision = match pipeline.route {
        CoverageRoute::Clarify { .. } => "clarify",
        CoverageRoute::HardMiss { .. } => "hard_miss",
        CoverageRoute::Select { .. } => "ready",
    }
    .to_string();
    let plan_select_exact = plan_select_matches_gold(
        pipeline,
        acceptable_sets,
        final_selection,
    );
    CoverageShadowMetrics {
        coverage_ambiguous,
        coverage_satisfiable,
        coverage_gold_recall,
        coverage_plan_recall: coverage_plan_recall_v,
        coverage_entity_recall: coverage_entity_recall_v,
        plan_route_decision,
        plan_select_exact,
        satisfiable_provider_count: pipeline.evaluation.satisfiable_plans_by_provider.len(),
        uncovered_count: pipeline.evaluation.uncovered.len(),
    }
}

fn plan_select_matches_gold(
    pipeline: &CoveragePipelineResult,
    acceptable_sets: &[Vec<(String, String)>],
    final_selection: Option<&SeedSelectionRaw>,
) -> bool {
    use crate::discovery_seed_select::SeedSelectionDecision;

    if acceptable_sets.is_empty() {
        return false;
    }
    let raw = match final_selection {
        Some(raw) => raw,
        None => match pipeline.selection.as_ref() {
            Some(raw) => raw,
            None => return false,
        },
    };
    if raw.decision != SeedSelectionDecision::Ready || raw.selected_ids.is_empty() {
        return false;
    }
    let selected = resolve_seeds_from_ids(&raw.selected_ids, &pipeline.evaluation);
    selection_matches_gold(&selected, acceptable_sets)
}

fn infer_catalog_route(allowed_entry_ids: &[String]) -> Vec<String> {
    if allowed_entry_ids.is_empty() {
        Vec::new()
    } else {
        allowed_entry_ids.to_vec()
    }
}

fn discover_catalog_route<C: CgsDiscovery>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: &[String],
) -> Vec<String> {
    let mut query = CapabilityQuery {
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

/// Build pipeline result from a completed evaluation (deterministic route + select).
pub fn build_coverage_pipeline(evaluation: CoverageEvaluation) -> CoveragePipelineResult {
    finalize_pipeline(evaluation)
}

/// Final seed selection raw for production path (coverage-first, deterministic).
pub fn coverage_first_selection_raw(pipeline: &CoveragePipelineResult) -> Option<SeedSelectionRaw> {
    pipeline.selection.clone()
}
