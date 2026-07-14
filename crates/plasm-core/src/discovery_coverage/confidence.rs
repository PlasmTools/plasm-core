//! Heuristic confidence for derive → optional upstream LLM slot extraction.

use indexmap::IndexMap;

use crate::schema::CGS;

use super::types::{
    CoverageEvaluation, DiscoveryCoveragePlan, ProviderConstraint, RequirementSlot,
};

const ENTITY_SCORE_EPS: i32 = 3;

/// Whether heuristic slot derivation is trusted or LLM slot extraction may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeriveConfidence {
    High,
    Low(LowConfidenceReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowConfidenceReason {
    MissingRootHint,
    WeakRootMargin,
    UncoveredWithPoolRecall,
    AmbiguousTaskRoot,
    RelationHopWithoutPlan,
}

impl DeriveConfidence {
    pub fn is_low(&self) -> bool {
        matches!(self, Self::Low(_))
    }
}

/// Collect entity phrase hits used by derive (for confidence + LLM glossary).
pub fn collect_derive_entity_hits(
    intent: &str,
    catalogs: &IndexMap<String, CGS>,
    allowed_entry_ids: &[String],
) -> Vec<(i32, String, String)> {
    super::derive::collect_entity_hits(intent, catalogs, allowed_entry_ids)
}

/// Score heuristic derive trust from plan + first-pass evaluation.
pub fn derive_confidence(
    plan: &DiscoveryCoveragePlan,
    evaluation: &CoverageEvaluation,
    entity_hits: &[(i32, String, String)],
) -> DeriveConfidence {
    let has_read_or_mutate = plan.slots.iter().any(|slot| {
        matches!(
            slot,
            RequirementSlot::ReadRoot { .. } | RequirementSlot::MutateAnchor { .. }
        )
    });
    if has_read_or_mutate {
        let root_hint = plan.slots.iter().find_map(|slot| match slot {
            RequirementSlot::ReadRoot { entity_hint } => entity_hint.clone(),
            RequirementSlot::MutateAnchor { entity_hint, .. } => entity_hint.clone(),
            _ => None,
        });
        if root_hint.is_none() {
            return DeriveConfidence::Low(LowConfidenceReason::MissingRootHint);
        }
        // Even with a committed hint, unbranded competing entity hits mean the hint is fragile.
        if matches!(plan.provider_constraint, ProviderConstraint::Unbranded)
            && weak_root_margin(entity_hits)
        {
            return DeriveConfidence::Low(LowConfidenceReason::WeakRootMargin);
        }
    }

    if !evaluation.uncovered.is_empty() && pool_covers_any_high_scoring_entity(evaluation) {
        return DeriveConfidence::Low(LowConfidenceReason::UncoveredWithPoolRecall);
    }

    if matches!(plan.provider_constraint, ProviderConstraint::Unbranded)
        && ambiguous_task_root(entity_hits)
    {
        return DeriveConfidence::Low(LowConfidenceReason::AmbiguousTaskRoot);
    }

    if plan
        .slots
        .iter()
        .any(|s| matches!(s, RequirementSlot::RelationHop { .. }))
        && evaluation.satisfiable_plans_by_provider.is_empty()
    {
        return DeriveConfidence::Low(LowConfidenceReason::RelationHopWithoutPlan);
    }

    DeriveConfidence::High
}

fn pool_covers_any_high_scoring_entity(evaluation: &CoverageEvaluation) -> bool {
    evaluation
        .bundles
        .iter()
        .any(|bundle| bundle.max_lexical_score >= 2)
}

fn ambiguous_task_root(entity_hits: &[(i32, String, String)]) -> bool {
    if entity_hits.len() < 2 {
        return false;
    }
    let top = entity_hits[0].0;
    let competitive: Vec<_> = entity_hits
        .iter()
        .filter(|(score, _, _)| top.saturating_sub(*score) <= ENTITY_SCORE_EPS)
        .map(|(_, entry_id, entity)| (entry_id.as_str(), entity.as_str()))
        .collect();
    let catalogs: std::collections::HashSet<_> = competitive.iter().map(|(c, _)| *c).collect();
    catalogs.len() >= 2 && competitive.len() >= 2
}

/// Top two distinct entity names score within ε — root pick is unreliable.
fn weak_root_margin(entity_hits: &[(i32, String, String)]) -> bool {
    if entity_hits.len() < 2 {
        return false;
    }
    let (top_score, _, top_entity) = &entity_hits[0];
    entity_hits.iter().skip(1).any(|(score, _, entity)| {
        !entity.eq_ignore_ascii_case(top_entity)
            && top_score.saturating_sub(*score) <= ENTITY_SCORE_EPS
    })
}
