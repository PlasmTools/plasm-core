//! Evaluate coverage plan: SeedPlans, evidence-based ambiguity, uncovered slots.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexMap;

use crate::schema::CGS;

use super::derive::derive_coverage_plan;
use super::enumerate::{enumerate_schema_bundles, enumerate_seed_plans, score_satisfiability};
use super::merge_slots::ground_slots;
use super::types::{
    CoverageEvaluation, DiscoveryCoveragePlan, ProviderAmbiguity, ProviderConstraint,
    RequirementSlot, SeedPlan, SeedSatisfiability, READY_MARGIN,
};

/// Full evaluation: derive plan, ground slots, enumerate bundles, build SeedPlans, detect ambiguity.
pub fn evaluate_coverage(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    catalog_route: &[String],
) -> CoverageEvaluation {
    let plan = derive_coverage_plan(intent, catalogs, allowed_entry_ids, catalog_route);
    evaluate_plan(&plan, intent, catalogs, allowed_entry_ids, catalog_route)
}

pub fn evaluate_plan(
    plan: &DiscoveryCoveragePlan,
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
    catalog_route: &[String],
) -> CoverageEvaluation {
    let plan = ground_slots(plan, intent, catalogs, allowed_entry_ids);
    let bundles = enumerate_schema_bundles(intent, catalogs, allowed_entry_ids, catalog_route);
    let scored = score_satisfiability(&plan, &bundles, catalogs);
    let mut satisfiable_plans_by_provider = enumerate_seed_plans(&plan, &scored, catalogs);

    for plans in satisfiable_plans_by_provider.values_mut() {
        plans.sort_by(|left, right| {
            left.seeds
                .len()
                .cmp(&right.seeds.len())
                .then_with(|| right.lexical_score.cmp(&left.lexical_score))
        });
    }

    let satisfiable_by_provider = flatten_seeds_from_plans(&satisfiable_plans_by_provider);
    let satisfiable_federation_tuples =
        federation_tuples_from_plans(&plan, &satisfiable_plans_by_provider);
    let uncovered = uncovered_slots(
        &plan,
        &satisfiable_plans_by_provider,
        &satisfiable_federation_tuples,
    );
    let ambiguity = detect_ambiguity(&plan, &satisfiable_plans_by_provider);

    CoverageEvaluation {
        plan,
        satisfiable_plans_by_provider,
        satisfiable_by_provider,
        satisfiable_federation_tuples,
        uncovered,
        ambiguity,
        bundles,
    }
}

fn flatten_seeds_from_plans(
    plans_by_provider: &BTreeMap<String, Vec<SeedPlan>>,
) -> BTreeMap<String, Vec<SeedSatisfiability>> {
    let mut out: BTreeMap<String, Vec<SeedSatisfiability>> = BTreeMap::new();
    for (provider, plans) in plans_by_provider {
        let mut seeds = Vec::new();
        for plan in plans {
            for seed in &plan.seeds {
                if !seeds
                    .iter()
                    .any(|existing: &SeedSatisfiability| existing.candidate_id == seed.candidate_id)
                {
                    seeds.push(seed.clone());
                }
            }
        }
        seeds.sort_by(|left, right| {
            right
                .lexical_score
                .cmp(&left.lexical_score)
                .then_with(|| left.entity.cmp(&right.entity))
        });
        out.insert(provider.clone(), seeds);
    }
    out
}

fn federation_tuples_from_plans(
    plan: &DiscoveryCoveragePlan,
    plans_by_provider: &BTreeMap<String, Vec<SeedPlan>>,
) -> Vec<Vec<SeedSatisfiability>> {
    let catalogs: Vec<String> = plan
        .slots
        .iter()
        .filter_map(|slot| match slot {
            RequirementSlot::FederateSlot { entry_id } => Some(entry_id.clone()),
            _ => None,
        })
        .collect();
    if catalogs.is_empty() {
        return Vec::new();
    }
    // Prefer an explicit multi-catalog federation plan if present.
    for plans in plans_by_provider.values() {
        for seed_plan in plans {
            let providers: HashSet<_> = seed_plan
                .seeds
                .iter()
                .map(|seed| seed.entry_id.as_str())
                .collect();
            if catalogs.iter().all(|c| providers.contains(c.as_str())) {
                return vec![seed_plan.seeds.clone()];
            }
        }
    }
    Vec::new()
}

fn uncovered_slots(
    plan: &DiscoveryCoveragePlan,
    plans_by_provider: &BTreeMap<String, Vec<SeedPlan>>,
    federation_tuples: &[Vec<SeedSatisfiability>],
) -> Vec<RequirementSlot> {
    let any_plan = plans_by_provider.values().any(|plans| !plans.is_empty());
    let federation_ok = plan
        .slots
        .iter()
        .any(|slot| matches!(slot, RequirementSlot::FederateSlot { .. }))
        && !federation_tuples.is_empty();
    if any_plan || federation_ok {
        return Vec::new();
    }
    plan.slots.clone()
}

fn detect_ambiguity(
    plan: &DiscoveryCoveragePlan,
    plans_by_provider: &BTreeMap<String, Vec<SeedPlan>>,
) -> ProviderAmbiguity {
    // Locked / rejected constraints are never "ambiguous providers" for clarify metrics.
    // Soft family unlock keeps Unbranded so can_ready can fail into clarify.
    if !matches!(plan.provider_constraint, ProviderConstraint::Unbranded) {
        return ProviderAmbiguity::None;
    }

    let mut viable: Vec<(String, &SeedPlan)> = plans_by_provider
        .iter()
        .filter_map(|(provider, plans)| plans.first().map(|plan| (provider.clone(), plan)))
        .collect();
    if viable.len() < 2 {
        return ProviderAmbiguity::None;
    }

    let coarse = coarse_slot_signature(plan);
    viable.retain(|(_, seed_plan)| {
        let plan_coarse = if seed_plan.slot_signature.is_empty() {
            coarse.clone()
        } else {
            coarse_from_signature(&seed_plan.slot_signature)
        };
        plan_coarse == coarse
    });
    if viable.len() < 2 {
        return ProviderAmbiguity::None;
    }

    // Dominant grounded margin → not ambiguous (can_ready will select).
    if has_grounded_root_hint(plan) {
        viable.sort_by(|left, right| right.1.lexical_score.cmp(&left.1.lexical_score));
        let top = viable[0].1.lexical_score;
        let second = viable.get(1).map(|(_, p)| p.lexical_score).unwrap_or(0);
        if top.saturating_sub(second) >= READY_MARGIN {
            return ProviderAmbiguity::None;
        }
    }

    viable.sort_by(|left, right| right.1.lexical_score.cmp(&left.1.lexical_score));
    ProviderAmbiguity::Between {
        providers: viable
            .iter()
            .map(|(provider, _)| provider.clone())
            .collect(),
        equivalent_plans: true,
    }
}

fn has_grounded_root_hint(plan: &DiscoveryCoveragePlan) -> bool {
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

fn coarse_slot_signature(plan: &DiscoveryCoveragePlan) -> String {
    let mut keys: Vec<String> = plan
        .slots
        .iter()
        .map(|slot| match slot {
            RequirementSlot::ReadRoot { .. } => "read".into(),
            RequirementSlot::RelationHop { target, .. } => format!("hop:{target}"),
            RequirementSlot::MutateAnchor { .. } => "mutate".into(),
            RequirementSlot::FederateSlot { entry_id } => format!("fed:{entry_id}"),
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys.join("|")
}

fn coarse_from_signature(signature: &str) -> String {
    let mut keys: Vec<String> = signature
        .split('|')
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.starts_with("read:") {
                "read".into()
            } else if let Some(rest) = part.strip_prefix("hop:") {
                // hop:wire:Target → hop:Target
                if let Some((_, target)) = rest.rsplit_once(':') {
                    format!("hop:{target}")
                } else {
                    format!("hop:{rest}")
                }
            } else if part.starts_with("mutate:") {
                "mutate".into()
            } else {
                part.to_string()
            }
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys.join("|")
}

/// Plan-based gold recall: some acceptable set is covered by some SeedPlan.
pub fn gold_in_satisfiable(
    evaluation: &CoverageEvaluation,
    acceptable_sets: &[Vec<(String, String)>],
) -> bool {
    coverage_plan_recall(evaluation, acceptable_sets)
}

/// ∃ acceptable_set. ∃ plan. plan.seeds covers every gold pair.
pub fn coverage_plan_recall(
    evaluation: &CoverageEvaluation,
    acceptable_sets: &[Vec<(String, String)>],
) -> bool {
    if acceptable_sets.is_empty() {
        return true;
    }
    let plans: Vec<&SeedPlan> = evaluation
        .satisfiable_plans_by_provider
        .values()
        .flatten()
        .collect();
    if plans.is_empty() {
        // Fall back to flat seeds for federation-only / transitional paths.
        let satisfiable: HashSet<_> = evaluation
            .satisfiable_by_provider
            .values()
            .flatten()
            .map(|s| (s.entry_id.as_str(), s.entity.as_str()))
            .collect();
        return acceptable_sets.iter().any(|gold| {
            gold.iter().all(|(entry_id, entity)| {
                satisfiable.contains(&(entry_id.as_str(), entity.as_str()))
            })
        });
    }
    acceptable_sets.iter().any(|gold| {
        plans.iter().any(|plan| {
            let chosen: HashSet<_> = plan
                .seeds
                .iter()
                .map(|s| (s.entry_id.as_str(), s.entity.as_str()))
                .collect();
            gold.iter()
                .all(|(entry_id, entity)| chosen.contains(&(entry_id.as_str(), entity.as_str())))
        })
    })
}

/// Diagnostic: each gold entity appears in the enumerated bundle pool.
pub fn coverage_entity_recall(
    evaluation: &CoverageEvaluation,
    acceptable_sets: &[Vec<(String, String)>],
) -> bool {
    if acceptable_sets.is_empty() {
        return true;
    }
    let pool: HashSet<_> = evaluation
        .bundles
        .iter()
        .map(|b| (b.entry_id.as_str(), b.entity.as_str()))
        .collect();
    acceptable_sets.iter().any(|gold| {
        gold.iter()
            .all(|(entry_id, entity)| pool.contains(&(entry_id.as_str(), entity.as_str())))
    })
}
