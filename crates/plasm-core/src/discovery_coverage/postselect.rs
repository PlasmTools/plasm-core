//! Post-select structural invariants on coverage ready selections.

use super::reasoning::format_coverage_reasoning;
use super::types::{CoverageEvaluation, CoverageRoute, RequirementSlot};
use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_candidate_graph::TypedCandidateGraph;
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_intent_signals::is_auxiliary_entity_for_mutation;
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::discovery_seed_select::{
    apply_seed_invariants_protected, supporting_capabilities_from_bundles, SeedSelectionDecision,
    SeedSelectionRaw,
};
use crate::discovery_seed_witness::{
    apply_teaching_satellites_to_ready, build_witness_corpus, DeterministicSeedPlan,
};

/// Apply graph-aware seed invariants to a coverage ready selection.
pub fn postprocess_coverage_selection(
    mut raw: SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: &CatalogWorkflowContext,
    candidate_graph: &TypedCandidateGraph,
    route: Option<&CoverageRoute>,
    evaluation: Option<&CoverageEvaluation>,
    intent: Option<&str>,
) -> SeedSelectionRaw {
    if raw.decision != SeedSelectionDecision::Ready {
        return raw;
    }
    let pre_ids = raw.selected_ids.clone();
    let protected = protected_roots_for_coverage(&pre_ids, bundles, evaluation);
    raw.selected_ids = apply_seed_invariants_protected(
        raw.selected_ids,
        bundles,
        intent_class,
        Some(catalog_context),
        Some(candidate_graph),
        &protected,
    );
    raw.supporting_capability_ids =
        supporting_capabilities_from_bundles(&raw.selected_ids, bundles);
    if let (Some(route), Some(evaluation)) = (route, evaluation) {
        raw.reasoning =
            format_coverage_reasoning(evaluation, route, Some(&pre_ids), Some(&raw.selected_ids));
    } else if pre_ids != raw.selected_ids {
        raw.reasoning = format!(
            "{} invariants={}→{}",
            raw.reasoning,
            pre_ids.join("+"),
            raw.selected_ids.join("+")
        );
    }
    let Some(corpus) =
        build_witness_corpus(bundles, &[], candidate_graph, Some(catalog_context))
    else {
        return raw;
    };
    let entities: Vec<(String, String)> = raw
        .selected_ids
        .iter()
        .filter_map(|id| {
            bundles
                .iter()
                .find(|b| &b.candidate_id == id)
                .map(|b| (b.entry_id.clone(), b.entity.clone()))
        })
        .collect();
    if entities.is_empty() {
        return raw;
    }
    let plan = DeterministicSeedPlan {
        symbol: "coverage_ready".into(),
        candidate_ids: raw.selected_ids.clone(),
        entities,
        lexical_score: 0,
        covered_witness_symbols: Vec::new(),
        summary: "coverage".into(),
    };
    let all: Vec<usize> = (0..corpus.witnesses.len()).collect();
    apply_teaching_satellites_to_ready(raw, &corpus, &plan, &all, intent)
}

/// Root hints from slots plus non-auxiliary entities already selected by coverage.
fn protected_roots_for_coverage(
    selected_ids: &[String],
    bundles: &[EntityCandidateBundle],
    evaluation: Option<&CoverageEvaluation>,
) -> Vec<String> {
    let mut protected = Vec::new();
    if let Some(evaluation) = evaluation {
        for slot in &evaluation.plan.slots {
            match slot {
                RequirementSlot::ReadRoot {
                    entity_hint: Some(hint),
                }
                | RequirementSlot::MutateAnchor {
                    entity_hint: Some(hint),
                    ..
                } => {
                    protected.push(hint.clone());
                }
                _ => {}
            }
        }
    }
    for id in selected_ids {
        let Some(bundle) = bundles.iter().find(|b| b.candidate_id == *id) else {
            continue;
        };
        if is_auxiliary_entity_for_mutation(&bundle.entity) {
            continue;
        }
        if !protected
            .iter()
            .any(|hint| hint.eq_ignore_ascii_case(&bundle.entity))
        {
            protected.push(bundle.entity.clone());
        }
    }
    protected
}
