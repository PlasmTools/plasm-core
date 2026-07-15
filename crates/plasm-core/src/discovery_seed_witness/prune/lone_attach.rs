//! Prefer a lone attach/dependent mutate over co-selected parents.

use std::collections::{HashMap, HashSet};

use super::super::corpus::{WitnessCorpus, WitnessKind};
use super::super::kind::CapBucket;
use super::super::named_in_intent::witness_named_in_intent;
use super::IntentGate;

/// Single attach/dependent Create|Update|Action + its parent Directs → keep leaf only.
/// Restores localized mutate when the LLM over-selects parent+leaf.
///
/// Under [`IntentGate::Strict`], demote parents only if the leaf is named by authored
/// discovery aliases — so co-selected dependent Creates do not steal a primary when
/// the intent is parent-list + relation nav (e.g. "show requested reviewers").
///
/// [`IntentGate::Ungated`] keeps the pre-gate demotion (test / explicit FO hazard).
pub(super) fn prefer_lone_attach_mutate_over_parents(
    corpus: &WitnessCorpus,
    selected: &[usize],
    intent: IntentGate<'_>,
) -> Vec<usize> {
    // Federated selections need primary anchors per catalog; do not collapse
    // parent+leaf to leaf alone across catalog boundaries.
    let catalogs: HashSet<&str> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter_map(|w| match &w.kind {
            WitnessKind::DirectCapability { entry_id, .. } => Some(entry_id.as_str()),
            _ => None,
        })
        .collect();
    if catalogs.len() >= 2 {
        return selected.to_vec();
    }

    let mut by_catalog: HashMap<&str, Vec<usize>> = HashMap::new();
    for &idx in selected {
        let Some(w) = corpus.witnesses.get(idx) else {
            continue;
        };
        let WitnessKind::DirectCapability { entry_id, .. } = &w.kind else {
            continue;
        };
        by_catalog.entry(entry_id.as_str()).or_default().push(idx);
    }

    let mut drop_idxs: HashSet<usize> = HashSet::new();

    for (_entry_id, idxs) in &by_catalog {
        let mut leaf_idxs: Vec<usize> = Vec::new();
        let mut leaf_entities: HashSet<&str> = HashSet::new();
        let mut other_idxs: Vec<usize> = Vec::new();
        for &idx in idxs {
            let Some(w) = corpus.witnesses.get(idx) else {
                continue;
            };
            let WitnessKind::DirectCapability {
                entity, kind, ..
            } = &w.kind
            else {
                other_idxs.push(idx);
                continue;
            };
            let leaf_mutate = (w.seed_nav.is_attach() || w.seed_class.is_dependent())
                && (CapBucket::is_create_or_update(kind) || CapBucket::is_action(kind));
            if leaf_mutate {
                leaf_idxs.push(idx);
                leaf_entities.insert(entity.as_str());
            } else {
                other_idxs.push(idx);
            }
        }
        if leaf_idxs.len() != 1 || other_idxs.is_empty() {
            continue;
        }
        let leaf_idx = leaf_idxs[0];
        let Some(leaf) = corpus.witnesses.get(leaf_idx) else {
            continue;
        };
        if matches!(intent, IntentGate::Strict(i) if !witness_named_in_intent(i, leaf)) {
            continue;
        }
        let parents: HashSet<&str> = leaf.pool.parent_entities().collect();
        if parents.is_empty() {
            continue;
        }
        // Sibling attach/dependent reads on the same leaf entity stay with the leaf.
        // Drop parent reads and parent Creates; keep parent Updates (multi-op spines).
        let parent_drop_idxs: Vec<usize> = other_idxs
            .iter()
            .copied()
            .filter(|&idx| {
                corpus.witnesses.get(idx).is_some_and(|w| match &w.kind {
                    WitnessKind::DirectCapability {
                        entity, kind, ..
                    } => {
                        if leaf_entities.contains(entity.as_str()) {
                            return false;
                        }
                        parents.contains(entity.as_str())
                            && (CapBucket::is_read_kind(kind) || CapBucket::is_create(kind))
                    }
                    _ => false,
                })
            })
            .collect();
        if parent_drop_idxs.is_empty() {
            continue;
        }
        let only_parents_or_leaf_sibs = other_idxs.iter().all(|&idx| {
            corpus.witnesses.get(idx).is_some_and(|w| match &w.kind {
                WitnessKind::DirectCapability {
                    entity, kind, ..
                } => {
                    leaf_entities.contains(entity.as_str())
                        || (parents.contains(entity.as_str())
                            && (CapBucket::is_read_kind(kind)
                                || CapBucket::is_create(kind)
                                || CapBucket::is_create_or_update(kind)))
                }
                _ => false,
            })
        });
        if !only_parents_or_leaf_sibs {
            continue;
        }
        for &idx in &parent_drop_idxs {
            drop_idxs.insert(idx);
        }
    }

    if drop_idxs.is_empty() {
        return selected.to_vec();
    }
    selected
        .iter()
        .copied()
        .filter(|idx| !drop_idxs.contains(idx))
        .collect()
}
