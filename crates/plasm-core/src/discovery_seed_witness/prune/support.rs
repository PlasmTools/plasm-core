//! Internal prune passes — stamps + pool topology only.

use std::collections::{HashMap, HashSet};

use super::super::corpus::{WitnessCorpus, WitnessKind};
use super::super::kind::CapBucket;
use super::super::roles::OwnEnd;

/// When Source and Target of an in-pool `own` edge are both DirectCapability-selected,
/// drop Target reads (Source is the collection/history owner).
pub(super) fn drop_redundant_own_targets(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
    let direct_entities: HashSet<(&str, &str)> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter_map(|w| match &w.kind {
            WitnessKind::DirectCapability {
                entry_id, entity, ..
            } => Some((entry_id.as_str(), entity.as_str())),
            _ => None,
        })
        .collect();

    let mut drop_targets: HashSet<(String, String)> = HashSet::new();
    let mut drop_ambient_sources: HashSet<(String, String)> = HashSet::new();
    for &idx in selected {
        let Some(w) = corpus.witnesses.get(idx) else {
            continue;
        };
        let WitnessKind::DirectCapability { entry_id, .. } = &w.kind else {
            continue;
        };
        if w.own_pairs.is_empty() {
            continue;
        }
        for edge in w.own_pairs.iter() {
            if direct_entities.contains(&(entry_id.as_str(), edge.source.as_str()))
                && direct_entities.contains(&(entry_id.as_str(), edge.target.as_str()))
            {
                // Ambient own-sources: keep the primary target.
                if corpus.roles.entity_is_ambient(entry_id, &edge.source) {
                    drop_ambient_sources.insert((entry_id.clone(), edge.source.clone()));
                } else {
                    drop_targets.insert((entry_id.clone(), edge.target.clone()));
                }
            }
        }
    }

    if drop_targets.is_empty() && drop_ambient_sources.is_empty() {
        return selected.to_vec();
    }

    selected
        .iter()
        .copied()
        .filter(|&idx| {
            let Some(w) = corpus.witnesses.get(idx) else {
                return false;
            };
            let WitnessKind::DirectCapability {
                entry_id,
                entity,
                kind,
                ..
            } = &w.kind
            else {
                return true;
            };
            if drop_ambient_sources.contains(&(entry_id.clone(), entity.clone())) {
                return false;
            }
            if !drop_targets.contains(&(entry_id.clone(), entity.clone())) {
                return true;
            }
            // Keep Target mutators (post/update); only XOR away history/list reads.
            !CapBucket::is_read_kind(kind)
        })
        .collect()
}

pub(super) fn drop_redundant_attach(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
    let cover = coverage_index(corpus, selected);
    selected
        .iter()
        .copied()
        .filter(|&idx| {
            let Some(w) = corpus.witnesses.get(idx) else {
                return false;
            };
            let WitnessKind::DirectCapability {
                entry_id,
                entity,
                kind,
                ..
            } = &w.kind
            else {
                return true;
            };
            if !w.seed_nav.is_attach() && !w.seed_class.is_dependent() {
                return true;
            }
            // Mutators on attach leaves (Create Comment, Action Pin, …) must remain as
            // *requirements* when a parent Direct is also selected. Plan seating may credit
            // parents for Create/reads (ParentPreferred); Actions stay leaf-seated. Teaching
            // satellites mint `e#` for leaves that parents cover.
            if !CapBucket::is_read_kind(kind) {
                return true;
            }
            let parents: Vec<&str> = w.pool.parent_entities().collect();
            if parents.is_empty() {
                return true;
            }
            let _ = entity;
            // Drop when any parent is already covered in the selection.
            !parents.iter().any(|parent| {
                cover
                    .get(&(entry_id.as_str(), *parent))
                    .is_some_and(|kinds| {
                        kinds.contains(&CoverKind::Direct) || kinds.contains(&CoverKind::HopFrom)
                    })
            })
        })
        .collect()
}

pub(super) fn drop_redundant_locate_ambient(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
    let cover = coverage_index(corpus, selected);
    let primary_by_catalog: HashSet<&str> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { .. }) && w.seed_class.is_primary()
        })
        .filter_map(|w| match &w.kind {
            WitnessKind::DirectCapability { entry_id, .. } => Some(entry_id.as_str()),
            _ => None,
        })
        .collect();

    selected
        .iter()
        .copied()
        .filter(|&idx| {
            let Some(w) = corpus.witnesses.get(idx) else {
                return false;
            };
            let WitnessKind::DirectCapability {
                entry_id, entity, ..
            } = &w.kind
            else {
                return true;
            };
            let _ = entity;
            let is_locate_source = w.seed_nav.is_locate();
            let is_ambient = w.seed_class.is_ambient();
            if !is_locate_source && !is_ambient {
                return true;
            }
            // Ambient is entity-level: drop when any primary DirectCapability in the same
            // catalog is selected (Repository may not declare an issues edge).
            if is_ambient && primary_by_catalog.contains(entry_id.as_str()) {
                return false;
            }
            let child_direct_selected = w.pool.child_targets().any(|child| {
                cover
                    .get(&(entry_id.as_str(), child))
                    .is_some_and(|kinds| kinds.contains(&CoverKind::Direct))
            });
            !child_direct_selected
        })
        .collect()
}

pub(super) fn promote_orphan_attach_reads(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
    // Group DirectCapabilities by catalog. Promote orphans *per catalog* so a
    // primary in another federated catalog does not veto attach→parent re-seat.
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
    if by_catalog.is_empty() {
        return selected.to_vec();
    }

    let mut drop_idxs: HashSet<usize> = HashSet::new();
    let mut parent_idxs: HashSet<usize> = HashSet::new();

    for (entry_id, idxs) in &by_catalog {
        let all_attach_dependent = idxs.iter().all(|&idx| {
            corpus.witnesses.get(idx).is_some_and(|w| {
                matches!(&w.kind, WitnessKind::DirectCapability { .. })
                    && (w.seed_nav.is_attach() || w.seed_class.is_dependent())
            })
        });
        let has_attach_read = idxs.iter().any(|&idx| {
            corpus.witnesses.get(idx).is_some_and(|w| match &w.kind {
                WitnessKind::DirectCapability { kind, .. } => {
                    CapBucket::is_read_kind(kind)
                        && (w.seed_nav.is_attach() || w.seed_class.is_dependent())
                }
                _ => false,
            })
        });
        // Promote when attach/dependent selection includes at least one read (history/list
        // nav). Lone Create/Update/Action stays leaf-seated for localized mutate.
        if !all_attach_dependent || !has_attach_read {
            continue;
        }

        let mut shared_parents: Option<HashSet<&str>> = None;
        for &idx in idxs {
            let Some(w) = corpus.witnesses.get(idx) else {
                continue;
            };
            let mut parents: HashSet<&str> = w.pool.parent_entities().collect();
            if parents.is_empty() {
                // Fallback: peers that declare this entity as an attach/own child.
                if let WitnessKind::DirectCapability { entity, .. } = &w.kind {
                    parents = peer_parents_via_child_edges(corpus, entry_id, entity);
                }
            }
            if parents.is_empty() {
                shared_parents = None;
                break;
            }
            shared_parents = Some(match shared_parents {
                None => parents,
                Some(prev) => prev.intersection(&parents).copied().collect(),
            });
        }
        let Some(shared) = shared_parents.filter(|s| !s.is_empty()) else {
            continue;
        };
        let parent_entity = if shared.len() == 1 {
            shared.into_iter().next()
        } else {
            shared
                .iter()
                .copied()
                .find(|parent| corpus.roles.entity_is_primary(entry_id, parent))
        };
        let Some(parent_entity) = parent_entity else {
            continue;
        };
        let Some(parent_idx) = best_read_direct_capability(corpus, entry_id, parent_entity) else {
            continue;
        };

        for &idx in idxs {
            drop_idxs.insert(idx);
        }
        parent_idxs.insert(parent_idx);
    }

    if drop_idxs.is_empty() {
        return selected.to_vec();
    }

    let mut out: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|idx| !drop_idxs.contains(idx))
        .collect();
    for parent_idx in parent_idxs {
        if !out.contains(&parent_idx) {
            out.push(parent_idx);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// When an own-end **target** Query/Get is selected without its source, promote the
/// in-pool source read. Falls back to a unique `pool` parent when `seed_nav=own` even
/// if `own_pairs` is empty (same-catalog source not stamped). Search/mutators stay.
pub(super) fn promote_orphan_own_target_reads(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
    let direct_entities: HashSet<(&str, &str)> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter_map(|w| match &w.kind {
            WitnessKind::DirectCapability {
                entry_id, entity, ..
            } => Some((entry_id.as_str(), entity.as_str())),
            _ => None,
        })
        .collect();

    let mut drop_idxs: HashSet<usize> = HashSet::new();
    let mut source_idxs: HashSet<usize> = HashSet::new();

    for &idx in selected {
        let Some(w) = corpus.witnesses.get(idx) else {
            continue;
        };
        let WitnessKind::DirectCapability {
            entry_id,
            entity,
            kind,
            ..
        } = &w.kind
        else {
            continue;
        };
        if !CapBucket::is_query_get(kind) {
            continue;
        }
        let role = w.own_pairs.end_role(entity);
        let own_target = matches!(role, OwnEnd::Target | OwnEnd::Both) || w.seed_nav.is_own();
        if !own_target {
            continue;
        }

        let mut unique_source: Option<String> = None;
        let mut skip = false;
        for edge in w.own_pairs.iter() {
            if edge.target != *entity {
                continue;
            }
            if direct_entities.contains(&(entry_id.as_str(), edge.source.as_str())) {
                skip = true;
                break;
            }
            match &unique_source {
                None => unique_source = Some(edge.source.clone()),
                Some(prev) if prev != &edge.source => {
                    skip = true;
                    break;
                }
                _ => {}
            }
        }
        if skip {
            continue;
        }
        if unique_source.is_none() && w.seed_nav.is_own() {
            let parents: Vec<&str> = w.pool.parent_entities().collect();
            if parents.len() == 1 {
                let parent = parents[0];
                if !direct_entities.contains(&(entry_id.as_str(), parent)) {
                    unique_source = Some(parent.to_string());
                }
            }
        }
        let Some(source_entity) = unique_source else {
            continue;
        };
        // Never elevate ambient own-sources over their targets.
        if corpus.roles.entity_is_ambient(entry_id, &source_entity) {
            continue;
        }
        let Some(source_idx) = best_read_direct_capability(corpus, entry_id, &source_entity) else {
            continue;
        };
        drop_idxs.insert(idx);
        source_idxs.insert(source_idx);
    }

    if drop_idxs.is_empty() {
        return selected.to_vec();
    }

    let mut out: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|idx| !drop_idxs.contains(idx))
        .collect();
    for source_idx in source_idxs {
        if !out.contains(&source_idx) {
            out.push(source_idx);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Sole ambient Direct(s) with a unique owned primary child → demote to that child.
/// Prevents ambient own-sources seating workflows alone when a unique primary child exists.
pub(super) fn demote_lone_ambient_to_own_primary(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
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
    let mut child_idxs: HashSet<usize> = HashSet::new();

    for (entry_id, idxs) in &by_catalog {
        let has_non_ambient = idxs.iter().any(|&idx| {
            corpus.witnesses.get(idx).is_some_and(|w| {
                matches!(&w.kind, WitnessKind::DirectCapability { .. })
                    && !w.seed_class.is_ambient()
            })
        });
        if has_non_ambient {
            continue;
        }
        for &idx in idxs {
            let Some(w) = corpus.witnesses.get(idx) else {
                continue;
            };
            let WitnessKind::DirectCapability { entity, .. } = &w.kind else {
                continue;
            };
            if !w.seed_class.is_ambient() {
                continue;
            }
            let mut targets: Vec<&str> = w
                .own_pairs
                .iter()
                .filter(|edge| edge.source == *entity)
                .map(|edge| edge.target.as_str())
                .collect();
            if targets.is_empty() {
                targets = w.pool.child_targets().collect();
            }
            targets.sort_unstable();
            targets.dedup();
            if targets.len() != 1 {
                continue;
            }
            let child = targets[0];
            let Some(child_idx) = best_read_direct_capability(corpus, entry_id, child) else {
                continue;
            };
            let Some(child_w) = corpus.witnesses.get(child_idx) else {
                continue;
            };
            if !child_w.seed_class.is_primary() {
                continue;
            }
            drop_idxs.insert(idx);
            child_idxs.insert(child_idx);
        }
    }

    if drop_idxs.is_empty() {
        return selected.to_vec();
    }
    let mut out: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|idx| !drop_idxs.contains(idx))
        .collect();
    for child_idx in child_idxs {
        if !out.contains(&child_idx) {
            out.push(child_idx);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// ≥2 attach/dependent Create|Update beside a same-catalog primary → drop leaves
/// (teaching satellites will re-admit). Preserves Action leaves and lone Create.
pub(super) fn demote_batch_attach_mutations_beside_primary(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
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

    for idxs in by_catalog.values() {
        let has_primary = idxs.iter().any(|&idx| {
            corpus.witnesses.get(idx).is_some_and(|w| {
                matches!(&w.kind, WitnessKind::DirectCapability { .. })
                    && w.seed_class.is_primary()
                    && !w.seed_nav.is_attach()
                    && !w.seed_class.is_dependent()
            })
        });
        if !has_primary {
            continue;
        }
        let mutate_leaves: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&idx| {
                let Some(w) = corpus.witnesses.get(idx) else {
                    return false;
                };
                let WitnessKind::DirectCapability { kind, .. } = &w.kind else {
                    return false;
                };
                (w.seed_nav.is_attach() || w.seed_class.is_dependent())
                    && CapBucket::is_create_or_update(kind)
            })
            .collect();
        if mutate_leaves.len() < 2 {
            continue;
        }
        // Shared unique parent → leave for `promote_shared_attach_mutations`.
        // Divergent parents (Comment→Issue, Label→Repository) beside a primary = demote.
        let mut common: Option<HashSet<&str>> = None;
        for &idx in &mutate_leaves {
            let Some(w) = corpus.witnesses.get(idx) else {
                common = Some(HashSet::new());
                break;
            };
            let parents: HashSet<&str> = w.pool.parent_entities().collect();
            common = Some(match common.take() {
                None => parents,
                Some(prev) => prev.intersection(&parents).copied().collect(),
            });
        }
        if common.as_ref().is_some_and(|c| c.len() == 1) {
            continue;
        }
        for &idx in &mutate_leaves {
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

/// When ≥2 attach/dependent Create/Update witnesses in one catalog share exactly one
/// typed parent, promote that parent and drop the leaves. Single-leaf localized Create
/// and Action leaves remain untouched.
pub(super) fn promote_shared_attach_mutations(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Vec<usize> {
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
    if by_catalog.is_empty() {
        return selected.to_vec();
    }

    let mut drop_idxs: HashSet<usize> = HashSet::new();
    let mut parent_idxs: HashSet<usize> = HashSet::new();

    for (entry_id, idxs) in &by_catalog {
        let mutate_leaves: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&idx| {
                let Some(w) = corpus.witnesses.get(idx) else {
                    return false;
                };
                let WitnessKind::DirectCapability { kind, .. } = &w.kind else {
                    return false;
                };
                (w.seed_nav.is_attach() || w.seed_class.is_dependent())
                    && CapBucket::is_create_or_update(kind)
            })
            .collect();
        if mutate_leaves.len() < 2 {
            continue;
        }

        let mut shared_parents: Option<HashSet<&str>> = None;
        let mut ok = true;
        for &idx in &mutate_leaves {
            let Some(w) = corpus.witnesses.get(idx) else {
                ok = false;
                break;
            };
            let parents: HashSet<&str> = w.pool.parent_entities().collect();
            if parents.is_empty() {
                ok = false;
                break;
            }
            shared_parents = Some(match shared_parents {
                None => parents,
                Some(prev) => prev.intersection(&parents).copied().collect(),
            });
        }
        if !ok {
            continue;
        }
        let Some(shared) = shared_parents.filter(|s| s.len() == 1) else {
            continue;
        };
        let Some(parent_entity) = shared.into_iter().next() else {
            continue;
        };
        let Some(parent_idx) = best_read_direct_capability(corpus, entry_id, parent_entity) else {
            continue;
        };

        for &idx in &mutate_leaves {
            drop_idxs.insert(idx);
        }
        parent_idxs.insert(parent_idx);
    }

    if drop_idxs.is_empty() {
        return selected.to_vec();
    }

    let mut out: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|idx| !drop_idxs.contains(idx))
        .collect();
    for parent_idx in parent_idxs {
        if !out.contains(&parent_idx) {
            out.push(parent_idx);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn best_read_direct_capability(
    corpus: &WitnessCorpus,
    entry_id: &str,
    entity: &str,
) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for (idx, w) in corpus.witnesses.iter().enumerate() {
        let WitnessKind::DirectCapability {
            entry_id: e,
            entity: ent,
            kind,
            ..
        } = &w.kind
        else {
            continue;
        };
        if e != entry_id || ent != entity || !CapBucket::is_read_kind(kind) {
            continue;
        }
        let rank = CapBucket::read_rank(kind);
        let score = w.lexical_score.saturating_add(rank);
        match best {
            None => best = Some((score, idx)),
            Some((prev, _)) if score > prev => best = Some((score, idx)),
            _ => {}
        }
    }
    best.map(|(_, idx)| idx)
}

fn peer_parents_via_child_edges<'a>(
    corpus: &'a WitnessCorpus,
    entry_id: &str,
    entity: &str,
) -> HashSet<&'a str> {
    let mut parents = HashSet::new();
    for w in &corpus.witnesses {
        let WitnessKind::DirectCapability {
            entry_id: e,
            entity: peer,
            ..
        } = &w.kind
        else {
            continue;
        };
        if e != entry_id || peer == entity {
            continue;
        }
        if w.pool.child_targets().any(|child| child == entity) {
            parents.insert(peer.as_str());
        }
    }
    parents
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CoverKind {
    Direct,
    HopFrom,
}

fn coverage_index<'a>(
    corpus: &'a WitnessCorpus,
    selected: &[usize],
) -> HashMap<(&'a str, &'a str), HashSet<CoverKind>> {
    let mut cover: HashMap<(&str, &str), HashSet<CoverKind>> = HashMap::new();
    for &idx in selected {
        let Some(w) = corpus.witnesses.get(idx) else {
            continue;
        };
        match &w.kind {
            WitnessKind::DirectCapability {
                entry_id, entity, ..
            } => {
                cover
                    .entry((entry_id.as_str(), entity.as_str()))
                    .or_default()
                    .insert(CoverKind::Direct);
            }
            WitnessKind::RelationHop {
                entry_id,
                from_entity,
                ..
            } => {
                cover
                    .entry((entry_id.as_str(), from_entity.as_str()))
                    .or_default()
                    .insert(CoverKind::HopFrom);
            }
        }
    }
    cover
}
