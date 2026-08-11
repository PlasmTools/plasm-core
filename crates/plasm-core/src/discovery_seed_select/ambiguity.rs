//! Deterministic provider ambiguity routing before semantic selection.

use std::collections::BTreeMap;

use crate::discovery_auto_seed::EntityCandidateBundle;

use super::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};

const MIN_PROVIDER_SCORE: u32 = 2;
/// Only treat catalogs as tied when their top entity scores are within this gap.
const MAX_COMPARABLE_SCORE_GAP: u32 = 1;

/// Abstain when an unbranded intent has similarly strong candidates in multiple catalogs.
///
/// Clarify only when discovery surfaced multiple providers on the catalog route with
/// near-tied top entities. A single dominant catalog (score gap or sole route hit)
/// proceeds to the selector.
pub fn deterministic_provider_ambiguity(
    bundles: &[EntityCandidateBundle],
    brand_lock_catalogs: &[String],
) -> Option<SeedSelectionRaw> {
    if !brand_lock_catalogs.is_empty() {
        return None;
    }

    let mut best_by_catalog: BTreeMap<&str, &EntityCandidateBundle> = BTreeMap::new();
    for bundle in bundles
        .iter()
        .filter(|bundle| !bundle.capabilities.is_empty())
    {
        best_by_catalog
            .entry(bundle.entry_id.as_str())
            .and_modify(|current| {
                if candidate_rank(bundle) > candidate_rank(current) {
                    *current = bundle;
                }
            })
            .or_insert(bundle);
    }

    let mut ranked: Vec<&EntityCandidateBundle> = best_by_catalog.values().copied().collect();
    ranked.sort_by(|left, right| candidate_rank(right).cmp(&candidate_rank(left)));

    let top = ranked.first().copied()?;
    if top.max_lexical_score < MIN_PROVIDER_SCORE {
        return None;
    }

    let routed: Vec<&EntityCandidateBundle> = ranked
        .iter()
        .copied()
        .filter(|bundle| bundle.catalog_route_evidence)
        .collect();
    if routed.len() < 2 {
        return None;
    }

    if let Some(runner_up) = ranked.get(1) {
        if is_dominant_leader(top, runner_up) {
            return None;
        }
    }

    let alternatives: Vec<SeedAlternativeSetRaw> = routed
        .iter()
        .copied()
        .filter(|bundle| scores_are_comparable(top, bundle))
        .map(|bundle| SeedAlternativeSetRaw {
            candidate_ids: vec![bundle.candidate_id.clone()],
            label: bundle.entry_id.clone(),
        })
        .collect();
    if alternatives.len() < 2 {
        return None;
    }

    Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: Vec::new(),
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        teaching_satellites: vec![],
        alternative_sets: alternatives,
        uncovered_requirements: Vec::new(),
        reasoning:
            "multiple routed catalogs have near-tied top entity matches and no catalog was named"
                .into(),
    })
}

fn scores_are_comparable(leader: &EntityCandidateBundle, other: &EntityCandidateBundle) -> bool {
    leader
        .max_lexical_score
        .saturating_sub(other.max_lexical_score)
        <= MAX_COMPARABLE_SCORE_GAP
}

fn is_dominant_leader(leader: &EntityCandidateBundle, runner_up: &EntityCandidateBundle) -> bool {
    if leader.max_lexical_score
        > runner_up
            .max_lexical_score
            .saturating_add(MAX_COMPARABLE_SCORE_GAP)
    {
        return true;
    }
    leader.catalog_route_evidence && !runner_up.catalog_route_evidence
}

fn candidate_rank(bundle: &EntityCandidateBundle) -> (u32, bool, usize, &str) {
    (
        bundle.max_lexical_score,
        bundle.catalog_route_evidence,
        bundle.capabilities.len(),
        bundle.candidate_id.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_auto_seed::EntityCapabilityEvidence;

    fn bundle(catalog: &str, entity: &str, score: u32, routed: bool) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: format!("{catalog}:{entity}"),
            entry_id: catalog.into(),
            entity: entity.into(),
            entity_description: String::new(),
            max_lexical_score: score,
            capabilities: vec![EntityCapabilityEvidence {
                capability_id: format!("{catalog}:{entity}:query"),
                capability_name: "query".into(),
                kind: "Query".into(),
                effect: crate::SemanticEffect::Read,
                description: String::new(),
                reason_codes: Vec::new(),
                lexical_score: score,
            }],
            relation_hints: String::new(),
            catalog_route_evidence: routed,
        }
    }

    #[test]
    fn near_tied_routed_providers_clarify() {
        let bundles = vec![
            bundle("gmail", "Thread", 5, true),
            bundle("outlook", "MailFolder", 5, true),
        ];
        let raw = deterministic_provider_ambiguity(&bundles, &[]).expect("clarify");
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
    }

    #[test]
    fn dominant_score_gap_does_not_clarify() {
        let bundles = vec![
            bundle("github", "PullRequest", 6, true),
            bundle("gitlab", "MergeRequest", 4, true),
        ];
        assert!(deterministic_provider_ambiguity(&bundles, &[]).is_none());
    }

    #[test]
    fn sole_routed_catalog_does_not_clarify() {
        let bundles = vec![
            bundle("github", "PullRequest", 5, true),
            bundle("gitlab", "MergeRequest", 5, false),
        ];
        assert!(deterministic_provider_ambiguity(&bundles, &[]).is_none());
    }

    #[test]
    fn brand_lock_or_weak_pool_does_not_clarify() {
        let tied = vec![
            bundle("catalog_a", "Item", 5, true),
            bundle("catalog_b", "Item", 5, true),
        ];
        assert!(deterministic_provider_ambiguity(&tied, &["catalog_a".into()]).is_none());

        let weak = vec![
            bundle("catalog_a", "Item", 1, true),
            bundle("catalog_b", "Item", 1, true),
        ];
        assert!(deterministic_provider_ambiguity(&weak, &[]).is_none());
    }
}
