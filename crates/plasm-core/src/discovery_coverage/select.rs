//! Minimal SeedPlan selection from coverage evaluation.

use std::collections::HashSet;

use super::types::{CoverageEvaluation, CoverageRoute, SeedPlan, SeedSatisfiability};

/// Select the minimal seed set for a routed provider (deterministic).
pub fn select_minimal_seed_set(
    evaluation: &CoverageEvaluation,
    route: &CoverageRoute,
) -> Vec<SeedSatisfiability> {
    match route {
        CoverageRoute::Select {
            selected,
            tie_candidates,
            plan,
            ..
        } => {
            if tie_candidates.len() >= 2 {
                return pick_minimal_from_ties(tie_candidates);
            }
            // Prefer the routed plan; fall back to selected seeds.
            if !plan.seeds.is_empty() {
                return prefer_non_auxiliary(plan.seeds.clone());
            }
            minimize_selected(evaluation, selected)
        }
        _ => Vec::new(),
    }
}

/// Choose the best SeedPlan among provider plans (fewest seeds, highest score).
pub fn select_best_plan(plans: &[SeedPlan]) -> Option<&SeedPlan> {
    plans.iter().min_by(|left, right| {
        left.seeds
            .len()
            .cmp(&right.seeds.len())
            .then_with(|| right.lexical_score.cmp(&left.lexical_score))
            .then_with(|| {
                left.candidate_ids()
                    .join(",")
                    .cmp(&right.candidate_ids().join(","))
            })
    })
}

fn minimize_selected(
    evaluation: &CoverageEvaluation,
    selected: &[SeedSatisfiability],
) -> Vec<SeedSatisfiability> {
    if selected.len() <= 1 {
        return prefer_non_auxiliary(selected.to_vec());
    }
    let has_federation = evaluation
        .plan
        .slots
        .iter()
        .any(|slot| matches!(slot, super::types::RequirementSlot::FederateSlot { .. }));
    if has_federation {
        return selected.to_vec();
    }
    if let Some(best) = selected
        .iter()
        .max_by_key(|seed| (seed.lexical_score, seed.entity.as_str()))
    {
        return vec![best.clone()];
    }
    selected.to_vec()
}

fn prefer_non_auxiliary(seeds: Vec<SeedSatisfiability>) -> Vec<SeedSatisfiability> {
    seeds
}

fn pick_minimal_from_ties(tie_candidates: &[Vec<SeedSatisfiability>]) -> Vec<SeedSatisfiability> {
    tie_candidates
        .iter()
        .min_by_key(|set| {
            (
                set.len(),
                set.iter()
                    .map(|seed| seed.entity.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            )
        })
        .cloned()
        .unwrap_or_default()
}

/// Check whether selected seeds match an acceptable gold set.
pub fn selection_matches_gold(
    selected: &[SeedSatisfiability],
    acceptable_sets: &[Vec<(String, String)>],
) -> bool {
    if acceptable_sets.is_empty() {
        return false;
    }
    let chosen: HashSet<_> = selected
        .iter()
        .map(|s| (s.entry_id.as_str(), s.entity.as_str()))
        .collect();
    acceptable_sets.iter().any(|gold| {
        gold.len() == chosen.len()
            && gold
                .iter()
                .all(|(entry_id, entity)| chosen.contains(&(entry_id.as_str(), entity.as_str())))
    })
}

/// Resolve satisfiability rows for candidate ids from a completed evaluation.
pub fn resolve_seeds_from_ids(
    ids: &[String],
    evaluation: &super::types::CoverageEvaluation,
) -> Vec<SeedSatisfiability> {
    use std::collections::HashMap;

    let mut by_id: HashMap<String, SeedSatisfiability> = HashMap::new();
    for seeds in evaluation.satisfiable_by_provider.values() {
        for seed in seeds {
            by_id
                .entry(seed.candidate_id.clone())
                .or_insert_with(|| seed.clone());
        }
    }
    for plans in evaluation.satisfiable_plans_by_provider.values() {
        for plan in plans {
            for seed in &plan.seeds {
                by_id
                    .entry(seed.candidate_id.clone())
                    .or_insert_with(|| seed.clone());
            }
        }
    }
    ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()
}
