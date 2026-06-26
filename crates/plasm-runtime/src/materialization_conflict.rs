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
        if content_diverged(
            base.relations.get(key),
            branch.relations.get(key),
            session.relations.get(key),
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
