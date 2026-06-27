//! CEP-14 three-way content divergence for optimistic branch commit validation.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use plasm_core::{Ref, TypedFieldValue};

use crate::cache::{CachedEntity, EntityCompleteness};

/// Per-key content captured lazily on first branch mutation (CEP-14).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntityBaseContent {
    pub fields: IndexMap<String, TypedFieldValue>,
    pub relations: IndexMap<String, Vec<Ref>>,
    pub completeness: EntityCompleteness,
}

impl EntityBaseContent {
    pub(crate) fn from_entity(entity: &CachedEntity) -> Self {
        Self {
            fields: entity.fields.clone(),
            relations: entity.relations.clone(),
            completeness: entity.completeness,
        }
    }

    pub(crate) fn content_equals(&self, entity: &CachedEntity) -> bool {
        self.fields == entity.fields
            && self.relations == entity.relations
            && self.completeness == entity.completeness
    }
}

/// Lazy fork-base tracker: only refs mutated during branch execute retain base snapshots.
#[derive(Debug, Clone)]
pub(crate) struct BranchForkTracker {
    pub(crate) initial_refs: HashSet<Ref>,
    pub(crate) lazy_base: HashMap<Ref, EntityBaseContent>,
}

impl BranchForkTracker {
    pub(crate) fn new(initial_refs: HashSet<Ref>) -> Self {
        Self {
            initial_refs,
            lazy_base: HashMap::new(),
        }
    }

    pub(crate) fn capture_if_needed(&mut self, reference: &Ref, existing: Option<&CachedEntity>) {
        if self.lazy_base.contains_key(reference) {
            return;
        }
        if let Some(entity) = existing {
            self.lazy_base
                .insert(reference.clone(), EntityBaseContent::from_entity(entity));
        }
    }
}

/// Canonical CEP-14 predicate: branch and session both diverged from base to different values.
#[must_use]
pub(crate) fn content_diverged<T: PartialEq + ?Sized>(
    base: Option<&T>,
    branch: Option<&T>,
    session: Option<&T>,
) -> bool {
    let branch_changed = branch != base;
    let session_changed = session != base;
    branch_changed && session_changed && branch != session
}

fn ref_set(refs: Option<&[Ref]>) -> HashSet<Ref> {
    refs.map(|v| v.iter().cloned().collect())
        .unwrap_or_default()
}

/// Deterministic union of relation/query ref lists (order-insensitive).
#[must_use]
pub fn union_sorted_refs(a: &[Ref], b: &[Ref]) -> Vec<Ref> {
    let mut set = ref_set(Some(a));
    set.extend(b.iter().cloned());
    let mut out: Vec<Ref> = set.into_iter().collect();
    out.sort_by_key(|r| r.to_string());
    out
}

/// CEP-15: relation/query ref-list materialization under concurrent reads.
///
/// Live execute often **replaces** a relation edge list with a fetched page. Two concurrent
/// branches that share an ancestor (e.g. `Type(electric).pokemon[…]`) materialize **different
/// overlapping pages** — union-mergeable, not a write conflict. Conflict only when fork-base
/// refs are retained inconsistently across branch vs session.
#[must_use]
pub(crate) fn ref_list_materialization_diverged(
    base: Option<&[Ref]>,
    branch: Option<&[Ref]>,
    session: Option<&[Ref]>,
) -> bool {
    if branch == session {
        return false;
    }
    let base_s = ref_set(base);
    let branch_s = ref_set(branch);
    let session_s = ref_set(session);

    if branch_s == base_s || session_s == base_s {
        // One side unchanged since fork — the other side's materialization wins.
        return false;
    }

    for r in base_s.iter() {
        if branch_s.contains(r) != session_s.contains(r) {
            return true;
        }
    }
    false
}

/// True when branch and session diverged from base on any field, relation, or completeness.
#[must_use]
pub(crate) fn entity_three_way_conflict(
    base: &EntityBaseContent,
    branch: &CachedEntity,
    session: &CachedEntity,
) -> bool {
    if content_diverged(
        Some(&base.completeness),
        Some(&branch.completeness),
        Some(&session.completeness),
    ) {
        return true;
    }

    let field_keys: std::collections::BTreeSet<&String> = base
        .fields
        .keys()
        .chain(branch.fields.keys())
        .chain(session.fields.keys())
        .collect();
    for key in field_keys {
        if content_diverged(
            base.fields.get(key),
            branch.fields.get(key),
            session.fields.get(key),
        ) {
            return true;
        }
    }

    let relation_keys: std::collections::BTreeSet<&String> = base
        .relations
        .keys()
        .chain(branch.relations.keys())
        .chain(session.relations.keys())
        .collect();
    for key in relation_keys {
        if ref_list_materialization_diverged(
            base.relations.get(key).map(|v| v.as_slice()),
            branch.relations.get(key).map(|v| v.as_slice()),
            session.relations.get(key).map(|v| v.as_slice()),
        ) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::GraphCache;
    use plasm_core::Value;

    fn berry(id: &str, name: &str) -> CachedEntity {
        let reference = Ref::new("Berry", id);
        let mut fields = IndexMap::new();
        fields.insert(
            "name".to_string(),
            TypedFieldValue::from(Value::String(name.to_string())),
        );
        CachedEntity {
            reference,
            fields,
            relations: IndexMap::new(),
            last_updated: 1,
            version: 1,
            completeness: EntityCompleteness::Complete,
        }
    }

    #[test]
    fn content_diverged_matches_cep14_formula() {
        assert!(!content_diverged(Some(&1), Some(&2), Some(&2)));
        assert!(!content_diverged(Some(&1), Some(&1), Some(&2)));
        assert!(content_diverged(Some(&1), Some(&2), Some(&3)));
    }

    #[test]
    fn ref_list_materialization_allows_concurrent_relation_pages() {
        let base = vec![Ref::new("Pokemon", "pikachu")];
        let page_a = vec![
            Ref::new("Pokemon", "1"),
            Ref::new("Pokemon", "2"),
        ];
        let page_b = vec![
            Ref::new("Pokemon", "1"),
            Ref::new("Pokemon", "3"),
        ];
        assert!(!ref_list_materialization_diverged(
            Some(base.as_slice()),
            Some(page_a.as_slice()),
            Some(page_b.as_slice()),
        ));
        assert!(!ref_list_materialization_diverged(
            Some(base.as_slice()),
            Some(page_a.as_slice()),
            Some(base.as_slice()),
        ));
    }

    #[test]
    fn ref_list_materialization_rejects_inconsistent_base_retention() {
        let base = vec![
            Ref::new("Pokemon", "a"),
            Ref::new("Pokemon", "b"),
        ];
        let branch = vec![
            Ref::new("Pokemon", "a"),
            Ref::new("Pokemon", "c"),
        ];
        let session = vec![
            Ref::new("Pokemon", "a"),
            Ref::new("Pokemon", "b"),
            Ref::new("Pokemon", "d"),
        ];
        assert!(ref_list_materialization_diverged(
            Some(base.as_slice()),
            Some(branch.as_slice()),
            Some(session.as_slice()),
        ));
    }

    #[test]
    fn lazy_fork_base_only_captures_touched_refs() {
        let mut session = GraphCache::new();
        for id in ["a", "b", "c", "d"] {
            session.insert(berry(id, id)).expect("seed");
        }
        let mut branch = session.fork_for_branch();
        let touched = Ref::new("Berry", "b");
        branch
            .insert({
                let mut e = berry("b", "mutated");
                e.version = 2;
                e
            })
            .expect("mutate b");

        let tracker = branch.branch_fork.as_ref().expect("tracker");
        assert_eq!(tracker.lazy_base.len(), 1);
        assert!(tracker.lazy_base.contains_key(&touched));

        let write_set = branch.branch_write_set();
        assert_eq!(write_set, vec![touched]);
        assert!(
            GraphCache::detect_branch_write_conflicts(&session, &branch, &write_set).is_empty()
        );
    }
}
