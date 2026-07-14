//! SeedSelectionRaw builders and federation completeness checks.

use std::collections::{HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_select::{
    supporting_capabilities_from_bundles, SeedAlternativeSetRaw, SeedSelectionDecision,
    SeedSelectionRaw,
};

use super::corpus::{witness_catalog, WitnessCorpus};
use super::plans::DeterministicSeedPlan;

/// Build ≥2 clarify alternative seed sets from the pool when the LLM abstains.
pub fn synthesize_clarify_alternatives(corpus: &WitnessCorpus) -> Vec<SeedAlternativeSetRaw> {
    let mut by_catalog: HashMap<String, &EntityCandidateBundle> = HashMap::new();
    for bundle in &corpus.bundles {
        by_catalog
            .entry(bundle.entry_id.clone())
            .and_modify(|best| {
                if bundle.max_lexical_score > best.max_lexical_score
                    || (bundle.max_lexical_score == best.max_lexical_score
                        && bundle.entity < best.entity)
                {
                    *best = bundle;
                }
            })
            .or_insert(bundle);
    }
    let mut catalogs: Vec<&EntityCandidateBundle> = by_catalog.into_values().collect();
    catalogs.sort_by(|a, b| {
        b.max_lexical_score
            .cmp(&a.max_lexical_score)
            .then_with(|| a.entry_id.cmp(&b.entry_id))
    });

    if catalogs.len() >= 2 {
        return catalogs
            .into_iter()
            .take(4)
            .map(|bundle| SeedAlternativeSetRaw {
                candidate_ids: vec![bundle.candidate_id.clone()],
                label: format!("{}.{}", bundle.entry_id, bundle.entity),
            })
            .collect();
    }

    let mut entities: Vec<&EntityCandidateBundle> = corpus.bundles.iter().collect();
    entities.sort_by(|a, b| {
        b.max_lexical_score
            .cmp(&a.max_lexical_score)
            .then_with(|| a.entity.cmp(&b.entity))
    });
    let mut alts = Vec::new();
    let mut seen_entity = HashSet::new();
    for bundle in entities {
        if !seen_entity.insert(bundle.entity.as_str()) {
            continue;
        }
        alts.push(SeedAlternativeSetRaw {
            candidate_ids: vec![bundle.candidate_id.clone()],
            label: format!("{}.{}", bundle.entry_id, bundle.entity),
        });
        if alts.len() >= 2 {
            break;
        }
    }
    alts
}

/// True when the intent explicitly named ≥2 catalogs but selected witnesses miss one.
pub fn missing_named_catalog_coverage(
    named_catalogs: &[String],
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<String> {
    if named_catalogs.len() < 2 || selected.is_empty() {
        return Vec::new();
    }
    let covered: HashSet<&str> = selected
        .iter()
        .map(|&idx| witness_catalog(&corpus.witnesses[idx]))
        .collect();
    named_catalogs
        .iter()
        .filter(|id| !covered.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Build ready selection from a single verified plan (no semantic rewriter).
pub fn selection_from_plan(
    corpus: &WitnessCorpus,
    plan: &DeterministicSeedPlan,
) -> SeedSelectionRaw {
    let selected_ids = plan.candidate_ids.clone();
    let supporting = supporting_capabilities_from_bundles(&selected_ids, &corpus.bundles);
    SeedSelectionRaw {
        decision: SeedSelectionDecision::Ready,
        requirements: plan
            .covered_witness_symbols
            .iter()
            .filter_map(|sym| corpus.witness(sym).map(|w| w.summary.clone()))
            .collect(),
        selected_ids,
        supporting_capability_ids: supporting,
        alternative_sets: vec![],
        uncovered_requirements: vec![],
        reasoning: format!(
            "multipass=ready plan={} seeds={}",
            plan.symbol, plan.summary
        ),
    }
}

/// Clarify with competing complete plans as alternative sets.
pub fn selection_clarify_from_plans(
    corpus: &WitnessCorpus,
    plans: &[DeterministicSeedPlan],
    reasoning: impl Into<String>,
) -> SeedSelectionRaw {
    let mut alternative_sets: Vec<SeedAlternativeSetRaw> = plans
        .iter()
        .map(|plan| SeedAlternativeSetRaw {
            candidate_ids: plan.candidate_ids.clone(),
            label: plan.summary.clone(),
        })
        .collect();
    if alternative_sets.len() < 2 {
        alternative_sets = synthesize_clarify_alternatives(corpus);
    }
    SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: vec![],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        alternative_sets,
        uncovered_requirements: vec![],
        reasoning: reasoning.into(),
    }
}

pub fn selection_hard_miss(
    uncovered: Vec<String>,
    reasoning: impl Into<String>,
) -> SeedSelectionRaw {
    SeedSelectionRaw {
        decision: SeedSelectionDecision::HardMiss,
        requirements: vec![],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        alternative_sets: vec![],
        uncovered_requirements: if uncovered.is_empty() {
            vec!["no covering seed plan".into()]
        } else {
            uncovered
        },
        reasoning: reasoning.into(),
    }
}
