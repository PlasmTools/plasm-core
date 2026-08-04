//! Route coverage evaluation to clarify, hard_miss, or select (SeedPlan-aware).

use super::reasoning::format_coverage_reasoning;
use super::types::{
    CoverageEvaluation, CoverageRoute, ProviderConstraint, RequirementSlot, SeedPlan,
    SeedSatisfiability, READY_MARGIN,
};
use crate::discovery_seed_select::{
    SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw,
};

/// Deterministic routing from coverage evaluation.
///
/// Ready only when [`can_ready`] holds. No soft "best score wins" when unbranded
/// multi-provider plans are within margin of each other.
pub fn route_coverage(evaluation: &CoverageEvaluation) -> CoverageRoute {
    route_coverage_with_margin(evaluation, READY_MARGIN)
}

/// Same as [`route_coverage`] with an explicit abstain margin (eval sweep only).
pub fn route_coverage_with_margin(evaluation: &CoverageEvaluation, margin: u32) -> CoverageRoute {
    let plan = &evaluation.plan;

    if evaluation.satisfiable_plans_by_provider.is_empty()
        && evaluation.satisfiable_federation_tuples.is_empty()
    {
        return CoverageRoute::HardMiss {
            uncovered: evaluation
                .uncovered
                .iter()
                .map(RequirementSlot::label)
                .collect(),
            reasoning: if !evaluation.uncovered.is_empty() {
                "no catalog satisfies derived requirements".into()
            } else {
                "no satisfiable seed plan".into()
            },
        };
    }

    if let ProviderConstraint::Locked(locked) = &plan.provider_constraint {
        if locked.len() == 1 {
            let provider = &locked[0];
            if let Some(plans) = evaluation.satisfiable_plans_by_provider.get(provider) {
                if let Some(best) = plans.first() {
                    return select_from_plan(best, provider, plans);
                }
            }
            if let Some(tuple) = evaluation.satisfiable_federation_tuples.first() {
                let required: Vec<_> = (0..plan.slots.len()).collect();
                if let Some(seed_plan) = SeedPlan::from_seeds(tuple.clone(), &required) {
                    return select_from_plan(
                        &seed_plan,
                        provider,
                        std::slice::from_ref(&seed_plan),
                    );
                }
            }
        } else if locked.len() > 1 {
            if let Some(tuple) = evaluation.satisfiable_federation_tuples.first() {
                let provider = tuple
                    .first()
                    .map(|s| s.entry_id.clone())
                    .unwrap_or_else(|| locked[0].clone());
                let required: Vec<_> = (0..plan.slots.len()).collect();
                if let Some(seed_plan) = SeedPlan::from_seeds(tuple.clone(), &required) {
                    return select_from_plan(
                        &seed_plan,
                        &provider,
                        std::slice::from_ref(&seed_plan),
                    );
                }
            }
            // Multi-catalog lock without a full federation tuple must not soft-ready
            // on a single provider (that is false_open for federation gold).
            let alternative_sets = clarify_alternatives_for_providers(evaluation, locked);
            if !alternative_sets.is_empty() {
                return CoverageRoute::Clarify {
                    alternative_sets,
                    reasoning:
                        "multi-catalog lock without satisfiable federation tuple; clarify to converge"
                            .into(),
                };
            }
            return CoverageRoute::HardMiss {
                uncovered: evaluation
                    .uncovered
                    .iter()
                    .map(RequirementSlot::label)
                    .collect(),
                reasoning: "multi-catalog lock unsatisfied".into(),
            };
        }
    }

    if can_ready_with_margin(evaluation, margin) {
        if let Some((provider, best, plans)) = best_provider_plan(evaluation) {
            return select_from_plan(best, provider, plans);
        }
        if let Some(tuple) = evaluation.satisfiable_federation_tuples.first() {
            let provider = tuple
                .first()
                .map(|seed| seed.entry_id.clone())
                .unwrap_or_default();
            let required: Vec<_> = (0..plan.slots.len()).collect();
            if let Some(seed_plan) = SeedPlan::from_seeds(tuple.clone(), &required) {
                return select_from_plan(&seed_plan, &provider, std::slice::from_ref(&seed_plan));
            }
        }
    }

    if !evaluation.satisfiable_plans_by_provider.is_empty()
        || !evaluation.satisfiable_federation_tuples.is_empty()
    {
        let alternative_sets = clarify_alternatives(evaluation);
        if !alternative_sets.is_empty() {
            return CoverageRoute::Clarify {
                alternative_sets,
                reasoning:
                    "cannot ready: unbranded multi-provider without grounded dominant margin".into(),
            };
        }
    }

    CoverageRoute::HardMiss {
        uncovered: evaluation
            .uncovered
            .iter()
            .map(RequirementSlot::label)
            .collect(),
        reasoning: "no satisfiable seed plan".into(),
    }
}

/// Ready iff locked single provider with a plan was already handled, or:
/// - exactly one satisfiable provider, or
/// - Unbranded with a grounded root hint and top-provider margin ≥ [`READY_MARGIN`].
pub fn can_ready(evaluation: &CoverageEvaluation) -> bool {
    can_ready_with_margin(evaluation, READY_MARGIN)
}

pub fn can_ready_with_margin(evaluation: &CoverageEvaluation, margin: u32) -> bool {
    let plan = &evaluation.plan;

    if let ProviderConstraint::Locked(locked) = &plan.provider_constraint {
        if locked.len() == 1 {
            return evaluation
                .satisfiable_plans_by_provider
                .get(&locked[0])
                .is_some_and(|plans| !plans.is_empty())
                || !evaluation.satisfiable_federation_tuples.is_empty();
        }
        return !evaluation.satisfiable_federation_tuples.is_empty()
            || evaluation.satisfiable_plans_by_provider.len() == 1;
    }

    let provider_count = evaluation.satisfiable_plans_by_provider.len();
    if provider_count == 1 {
        return true;
    }
    if provider_count == 0 {
        return !evaluation.satisfiable_federation_tuples.is_empty();
    }

    if !has_grounded_root_hint(plan) {
        return false;
    }

    let mut scores: Vec<u32> = evaluation
        .satisfiable_plans_by_provider
        .values()
        .filter_map(|plans| plans.first().map(|p| p.lexical_score))
        .collect();
    if scores.len() < 2 {
        return scores.len() == 1;
    }
    scores.sort_by(|a, b| b.cmp(a));
    scores[0].saturating_sub(scores[1]) >= margin
}

fn has_grounded_root_hint(plan: &super::types::DiscoveryCoveragePlan) -> bool {
    plan.slots.iter().any(|slot| {
        matches!(
            slot,
            RequirementSlot::ReadRoot {
                entity_hint: Some(_)
            } | RequirementSlot::MutateAnchor {
                entity_hint: Some(_),
                ..
            }
        )
    })
}

fn best_provider_plan(evaluation: &CoverageEvaluation) -> Option<(&str, &SeedPlan, &[SeedPlan])> {
    let mut ranked: Vec<(&String, &SeedPlan)> = evaluation
        .satisfiable_plans_by_provider
        .iter()
        .filter_map(|(provider, plans)| plans.first().map(|plan| (provider, plan)))
        .collect();
    ranked.sort_by(|left, right| {
        left.1
            .seeds
            .len()
            .cmp(&right.1.seeds.len())
            .then_with(|| right.1.lexical_score.cmp(&left.1.lexical_score))
    });
    let (provider, best) = ranked.first()?;
    let plans = evaluation
        .satisfiable_plans_by_provider
        .get(*provider)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Some((provider.as_str(), *best, plans))
}

fn clarify_alternatives(evaluation: &CoverageEvaluation) -> Vec<SeedAlternativeSetRaw> {
    clarify_alternatives_for_providers(evaluation, &[])
}

fn clarify_alternatives_for_providers(
    evaluation: &CoverageEvaluation,
    restrict_to: &[String],
) -> Vec<SeedAlternativeSetRaw> {
    let mut ranked: Vec<(&String, &SeedPlan)> = evaluation
        .satisfiable_plans_by_provider
        .iter()
        .filter(|(provider, _)| {
            restrict_to.is_empty() || restrict_to.iter().any(|id| id == *provider)
        })
        .filter_map(|(provider, plans)| plans.first().map(|plan| (provider, plan)))
        .collect();
    ranked.sort_by_key(|right| std::cmp::Reverse(right.1.lexical_score));
    ranked
        .into_iter()
        .map(|(entry_id, seed_plan)| SeedAlternativeSetRaw {
            candidate_ids: seed_plan.candidate_ids(),
            label: format!(
                "{entry_id}:{}",
                seed_plan
                    .seeds
                    .first()
                    .map(|s| s.entity.as_str())
                    .unwrap_or("?")
            ),
        })
        .collect()
}

fn select_from_plan(best: &SeedPlan, provider: &str, plans: &[SeedPlan]) -> CoverageRoute {
    let tie_candidates = tie_plans(plans);
    CoverageRoute::Select {
        selected: best.seeds.clone(),
        provider: provider.to_string(),
        tie_candidates,
        plan: best.clone(),
    }
}

fn tie_plans(plans: &[SeedPlan]) -> Vec<Vec<SeedSatisfiability>> {
    if plans.len() <= 1 {
        return Vec::new();
    }
    let top = plans[0].lexical_score;
    let same_len = plans[0].seeds.len();
    let tied: Vec<_> = plans
        .iter()
        .filter(|plan| plan.seeds.len() == same_len && top.saturating_sub(plan.lexical_score) <= 1)
        .map(|plan| plan.seeds.clone())
        .collect();
    if tied.len() >= 2 {
        tied
    } else {
        Vec::new()
    }
}

pub fn route_to_selection_raw(
    route: &CoverageRoute,
    evaluation: &CoverageEvaluation,
) -> SeedSelectionRaw {
    let reasoning = format_coverage_reasoning(evaluation, route, None, None);
    match route {
        CoverageRoute::Clarify {
            alternative_sets, ..
        } => SeedSelectionRaw {
            decision: SeedSelectionDecision::Clarify,
            requirements: Vec::new(),
            selected_ids: Vec::new(),
            supporting_capability_ids: Vec::new(),
            teaching_satellites: vec![],
            alternative_sets: alternative_sets.clone(),
            uncovered_requirements: Vec::new(),
            reasoning,
        },
        CoverageRoute::HardMiss { uncovered, .. } => SeedSelectionRaw {
            decision: SeedSelectionDecision::HardMiss,
            requirements: Vec::new(),
            selected_ids: Vec::new(),
            supporting_capability_ids: Vec::new(),
            teaching_satellites: vec![],
            alternative_sets: Vec::new(),
            uncovered_requirements: uncovered.clone(),
            reasoning,
        },
        CoverageRoute::Select { selected, .. } => {
            let selected_ids: Vec<String> =
                selected.iter().map(|s| s.candidate_id.clone()).collect();
            let supporting_capability_ids: Vec<String> = selected
                .iter()
                .flat_map(|s| {
                    s.bundle
                        .capabilities
                        .iter()
                        .map(|cap| cap.capability_id.clone())
                })
                .collect();
            SeedSelectionRaw {
                decision: SeedSelectionDecision::Ready,
                requirements: Vec::new(),
                selected_ids,
                supporting_capability_ids,
                teaching_satellites: Vec::new(),
                alternative_sets: Vec::new(),
                uncovered_requirements: Vec::new(),
                reasoning,
            }
        }
    }
}
