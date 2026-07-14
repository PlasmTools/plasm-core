//! Human-readable selection reasoning for traces and eval attribution.

use super::types::{CoverageEvaluation, CoverageRoute, RequirementSlot, SeedSatisfiability};

/// Build a compact, causal reasoning string from coverage state.
pub fn format_coverage_reasoning(
    evaluation: &CoverageEvaluation,
    route: &CoverageRoute,
    pre_invariant_ids: Option<&[String]>,
    post_invariant_ids: Option<&[String]>,
) -> String {
    let slots = format_slots(&evaluation.plan.slots);
    let provider = format_provider(&evaluation.plan.provider_constraint);
    let mut parts = vec![match route {
        CoverageRoute::Clarify { .. } => "route=clarify".into(),
        CoverageRoute::HardMiss { .. } => "route=hard_miss".into(),
        CoverageRoute::Select { provider, .. } => format!("route=ready provider={provider}"),
    }];
    parts.push(format!("slots=[{slots}]"));
    parts.push(format!("lock={provider}"));

    match route {
        CoverageRoute::Clarify {
            alternative_sets, ..
        } => {
            let alts: Vec<_> = alternative_sets
                .iter()
                .map(|set| set.label.clone())
                .collect();
            parts.push(format!("alternatives=[{}]", alts.join("|")));
        }
        CoverageRoute::HardMiss { uncovered, .. } => {
            if uncovered.is_empty() {
                parts.push(format!(
                    "uncovered=[{}]",
                    evaluation
                        .uncovered
                        .iter()
                        .map(RequirementSlot::label)
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            } else {
                parts.push(format!("uncovered=[{}]", uncovered.join(",")));
            }
            parts.push(format!(
                "satisfiable_providers={}",
                evaluation.satisfiable_plans_by_provider.len()
            ));
        }
        CoverageRoute::Select {
            plan,
            tie_candidates,
            ..
        } => {
            parts.push(format!(
                "plan=[{}] score={} ties={}",
                format_seed_set(&plan.seeds),
                plan.lexical_score,
                tie_candidates.len()
            ));
            if let Some(ids) = pre_invariant_ids {
                parts.push(format!("selected=[{}]", ids.join("+")));
            } else {
                parts.push(format!("selected=[{}]", format_seed_ids(&plan.seeds)));
            }
        }
    }

    if let (Some(pre), Some(post)) = (pre_invariant_ids, post_invariant_ids) {
        if pre != post {
            parts.push(format!("invariants={}→{}", pre.join("+"), post.join("+")));
        }
    }

    parts.join(" ")
}

fn format_slots(slots: &[RequirementSlot]) -> String {
    slots
        .iter()
        .map(RequirementSlot::label)
        .collect::<Vec<_>>()
        .join(",")
}

fn format_provider(constraint: &super::types::ProviderConstraint) -> String {
    match constraint {
        super::types::ProviderConstraint::Unbranded => "unbranded".into(),
        super::types::ProviderConstraint::Locked(ids) => ids.join("|"),
        super::types::ProviderConstraint::Rejected(ids) => format!("reject:{}", ids.join("|")),
    }
}

fn format_seed_set(seeds: &[SeedSatisfiability]) -> String {
    seeds
        .iter()
        .map(|seed| format!("{}:{}", seed.entry_id, seed.entity))
        .collect::<Vec<_>>()
        .join("+")
}

fn format_seed_ids(seeds: &[SeedSatisfiability]) -> String {
    seeds
        .iter()
        .map(|seed| seed.candidate_id.as_str())
        .collect::<Vec<_>>()
        .join("+")
}
