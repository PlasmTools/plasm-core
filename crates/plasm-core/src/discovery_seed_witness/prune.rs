//! Graph+role witness prune — catalog-authored `seed_nav` / `seed_class` only.

use std::collections::{HashMap, HashSet};

use super::corpus::{RequirementWitness, WitnessCorpus, WitnessKind};

/// Drop redundant attach/ambient/own-target witnesses and promote orphan attach reads.
///
/// Uses only roles stamped on [`RequirementWitness`] at corpus build time
/// (catalog-authored) plus typed pool links / witness kinds — never entity English.
///
/// `own` XOR: when DirectCapabilities on both ends of an `own` edge are selected,
/// drop Target **reads** and keep Source (mutators on Target remain).
pub fn prune_witness_selection(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
    if selected.is_empty() {
        return Vec::new();
    }
    let mut selected: Vec<usize> = selected.to_vec();
    selected = drop_redundant_attach(corpus, &selected);
    selected = drop_redundant_own_targets(corpus, &selected);
    selected = drop_redundant_locate_ambient(corpus, &selected);
    selected = promote_orphan_attach_reads(corpus, &selected);
    selected
}

/// When Source and Target of an in-pool `own` edge are both DirectCapability-selected,
/// drop Target reads (Source is the collection/history owner).
fn drop_redundant_own_targets(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
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
                drop_targets.insert((entry_id.clone(), edge.target.clone()));
            }
        }
    }

    if drop_targets.is_empty() {
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
            if !drop_targets.contains(&(entry_id.clone(), entity.clone())) {
                return true;
            }
            // Keep Target mutators (post/update); only XOR away history/list reads.
            !is_read_kind(kind)
        })
        .collect()
}

fn drop_redundant_attach(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
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
            // Mutators on attach leaves (Create Comment, …) must remain — parent seeds
            // do not cover child mutate witnesses in plan math.
            if !is_read_kind(kind) {
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

fn drop_redundant_locate_ambient(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
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

fn promote_orphan_attach_reads(corpus: &WitnessCorpus, selected: &[usize]) -> Vec<usize> {
    let directs: Vec<&RequirementWitness> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter(|w| matches!(w.kind, WitnessKind::DirectCapability { .. }))
        .collect();
    if directs.is_empty() {
        return selected.to_vec();
    }
    let all_orphan_attach = directs.iter().all(|w| {
        matches!(
            &w.kind,
            WitnessKind::DirectCapability { kind, .. }
                if is_read_kind(kind)
        ) && (w.seed_nav.is_attach() || w.seed_class.is_dependent())
    });
    if !all_orphan_attach {
        return selected.to_vec();
    }

    let mut shared_parents: Option<HashSet<(String, String)>> = None;
    for w in &directs {
        let WitnessKind::DirectCapability {
            entry_id, entity, ..
        } = &w.kind
        else {
            continue;
        };
        let _ = entity;
        let parents: HashSet<(String, String)> = w
            .pool
            .parent_entities()
            .map(|p| (entry_id.clone(), p.to_string()))
            .collect();
        if parents.is_empty() {
            return selected.to_vec();
        }
        shared_parents = Some(match shared_parents {
            None => parents,
            Some(prev) => prev.intersection(&parents).cloned().collect(),
        });
    }
    let Some(shared) = shared_parents.filter(|s| s.len() == 1) else {
        return selected.to_vec();
    };
    let Some((parent_entry, parent_entity)) = shared.into_iter().next() else {
        return selected.to_vec();
    };

    let Some(parent_idx) = best_read_direct_capability(corpus, &parent_entry, &parent_entity)
    else {
        return selected.to_vec();
    };

    let mut out: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|&idx| {
            let Some(w) = corpus.witnesses.get(idx) else {
                return false;
            };
            !matches!(&w.kind, WitnessKind::DirectCapability { .. })
        })
        .collect();
    if !out.contains(&parent_idx) {
        out.push(parent_idx);
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
        if e != entry_id || ent != entity || !is_read_kind(kind) {
            continue;
        }
        let rank = read_kind_rank(kind);
        let score = w.lexical_score.saturating_add(rank);
        match best {
            None => best = Some((score, idx)),
            Some((prev, _)) if score > prev => best = Some((score, idx)),
            _ => {}
        }
    }
    best.map(|(_, idx)| idx)
}

fn is_read_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Query" | "Search" | "Get" | "query" | "search" | "get"
    )
}

fn read_kind_rank(kind: &str) -> u32 {
    match kind {
        "Query" | "query" => 30,
        "Search" | "search" => 20,
        "Get" | "get" => 10,
        _ => 0,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    use super::super::roles::{
        OwnEdge, OwnPairs, PoolChild, PoolLinks, SeedClassStamp, SeedNavStamp,
    };
    use crate::schema::{DiscoverySeedClass, DiscoverySeedNav};

    fn class(s: &str) -> SeedClassStamp {
        match s {
            "primary" => SeedClassStamp::Authored(DiscoverySeedClass::Primary),
            "dependent" => SeedClassStamp::Authored(DiscoverySeedClass::Dependent),
            "ambient" => SeedClassStamp::Authored(DiscoverySeedClass::Ambient),
            _ => SeedClassStamp::Unset,
        }
    }

    fn nav(s: &str) -> SeedNavStamp {
        match s {
            "attach" => SeedNavStamp::Authored(DiscoverySeedNav::Attach),
            "own" => SeedNavStamp::Authored(DiscoverySeedNav::Own),
            "locate" => SeedNavStamp::Authored(DiscoverySeedNav::Locate),
            _ => SeedNavStamp::Unset,
        }
    }

    fn pool_from_note(graph_note: &str) -> PoolLinks {
        let mut parents = BTreeSet::new();
        let mut children = BTreeSet::new();
        for part in graph_note.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix("relation_child_of=") {
                for p in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                    parents.insert(p.to_string());
                }
            } else if let Some(rest) = part.strip_prefix("relation_anchor_to=") {
                for item in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                    if let Some((wire, target)) = item.split_once(':') {
                        children.insert(PoolChild::new(wire, target));
                    } else {
                        children.insert(PoolChild::new("", item));
                    }
                }
            }
        }
        PoolLinks {
            parents,
            children,
            siblings: BTreeSet::new(),
        }
    }

    fn own_from_label(own_pair: &str) -> OwnPairs {
        if own_pair.is_empty() || own_pair == "unset" {
            return OwnPairs::default();
        }
        OwnPairs::new(own_pair.split('|').filter_map(|part| {
            let part = part.trim();
            let (source, target) = part.split_once('→').or_else(|| part.split_once("->"))?;
            Some(OwnEdge::new(source.trim(), target.trim()))
        }))
    }

    fn direct(
        entry: &str,
        entity: &str,
        kind: &str,
        seed_class: &str,
        seed_nav: &str,
        graph_note: &str,
        score: u32,
    ) -> RequirementWitness {
        direct_with_own(
            entry, entity, kind, seed_class, seed_nav, "unset", graph_note, score,
        )
    }

    fn direct_with_own(
        entry: &str,
        entity: &str,
        kind: &str,
        seed_class: &str,
        seed_nav: &str,
        own_pair: &str,
        graph_note: &str,
        score: u32,
    ) -> RequirementWitness {
        RequirementWitness {
            symbol: String::new(),
            kind: WitnessKind::DirectCapability {
                entry_id: entry.into(),
                entity: entity.into(),
                capability_id: format!("{entry}:{entity}:{kind}"),
                capability_name: format!("{entity}_{kind}"),
                kind: kind.into(),
                description: format!("{kind} {entity}"),
            },
            owner_candidate_id: format!("{entry}:{entity}"),
            lexical_score: score,
            summary: format!("{kind} {entity}"),
            entity_description: format!("{entity} desc"),
            aliases: entity.to_ascii_lowercase(),
            pool: pool_from_note(graph_note),
            seed_class: class(seed_class),
            seed_nav: nav(seed_nav),
            own_pairs: own_from_label(own_pair),
        }
    }

    fn corpus(mut witnesses: Vec<RequirementWitness>) -> WitnessCorpus {
        let mut symbol_to_index = HashMap::new();
        for (idx, w) in witnesses.iter_mut().enumerate() {
            w.symbol = format!("w{}", idx + 1);
            symbol_to_index.insert(w.symbol.clone(), idx);
        }
        WitnessCorpus {
            witnesses,
            bundles: vec![],
            brand_lock_catalogs: vec![],
            symbol_to_index,
        }
    }

    #[test]
    fn drops_attach_when_parent_also_selected() {
        let corpus = corpus(vec![
            direct(
                "fx",
                "Issue",
                "Query",
                "primary",
                "unset",
                "relation_anchor_to=comments:Comment",
                80,
            ),
            direct(
                "fx",
                "Comment",
                "Query",
                "dependent",
                "attach",
                "relation_child_of=Issue",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![0]);
    }

    #[test]
    fn drops_attach_label_read_when_issue_selected() {
        let corpus = corpus(vec![
            direct(
                "fx",
                "Issue",
                "Query",
                "primary",
                "unset",
                "relation_anchor_to=labels:Label",
                80,
            ),
            direct(
                "fx",
                "Label",
                "Query",
                "dependent",
                "attach",
                "relation_child_of=Issue",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![0]);
    }

    #[test]
    fn promotes_orphan_attach_read_to_parent() {
        let corpus = corpus(vec![
            direct(
                "fx",
                "Issue",
                "Query",
                "primary",
                "unset",
                "relation_anchor_to=comments:Comment",
                80,
            ),
            direct(
                "fx",
                "Comment",
                "Query",
                "dependent",
                "attach",
                "relation_child_of=Issue",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[1]);
        assert_eq!(pruned, vec![0]);
    }

    #[test]
    fn drops_ambient_when_child_direct_selected() {
        let corpus = corpus(vec![
            direct(
                "fx",
                "Repository",
                "Query",
                "ambient",
                "locate",
                "relation_anchor_to=issues:Issue",
                50,
            ),
            direct(
                "fx",
                "Issue",
                "Query",
                "primary",
                "unset",
                "relation_child_of=Repository",
                80,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![1]);
    }

    #[test]
    fn drops_ambient_when_same_catalog_primary_without_edge() {
        let corpus = corpus(vec![
            direct("fx", "Repository", "Query", "ambient", "unset", "", 50),
            direct("fx", "Issue", "Query", "primary", "unset", "", 80),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![1]);
    }

    #[test]
    fn keeps_attach_mutate_when_parent_selected() {
        let corpus = corpus(vec![
            direct(
                "fx",
                "Issue",
                "Query",
                "primary",
                "unset",
                "relation_anchor_to=comments:Comment",
                80,
            ),
            direct(
                "fx",
                "Comment",
                "Create",
                "dependent",
                "attach",
                "relation_child_of=Issue",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![0, 1]);
    }

    #[test]
    fn own_edge_xor_keeps_source_drops_target_read() {
        let corpus = corpus(vec![
            direct_with_own(
                "fx",
                "Channel",
                "Query",
                "primary",
                "unset",
                "Channel→Message",
                "relation_anchor_to=messages:Message",
                80,
            ),
            direct_with_own(
                "fx",
                "Message",
                "Query",
                "primary",
                "own",
                "Channel→Message",
                "relation_child_of=Channel",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![0], "own XOR keeps Source read only");
    }

    #[test]
    fn own_edge_xor_keeps_target_mutate_with_source() {
        let corpus = corpus(vec![
            direct_with_own(
                "fx",
                "Channel",
                "Query",
                "primary",
                "unset",
                "Channel→Message",
                "relation_anchor_to=messages:Message",
                80,
            ),
            direct_with_own(
                "fx",
                "Message",
                "Create",
                "primary",
                "own",
                "Channel→Message",
                "relation_child_of=Channel",
                70,
            ),
        ]);
        let pruned = prune_witness_selection(&corpus, &[0, 1]);
        assert_eq!(pruned, vec![0, 1], "Target mutate survives own XOR");
    }
}
