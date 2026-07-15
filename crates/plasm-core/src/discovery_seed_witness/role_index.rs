//! Precomputed seed-class roles for a [`WitnessCorpus`] — one scan, shared queries.

use std::collections::HashSet;

use super::corpus::{RequirementWitness, WitnessKind};

/// Ambient / primary DirectCapability owners and (entry, entity) pairs.
#[derive(Debug, Clone, Default)]
pub struct CorpusRoleIndex {
    ambient_entities: HashSet<(String, String)>,
    primary_entities: HashSet<(String, String)>,
    ambient_owners: HashSet<String>,
    primary_owners: HashSet<String>,
}

impl CorpusRoleIndex {
    pub fn build(witnesses: &[RequirementWitness]) -> Self {
        let mut idx = Self::default();
        for w in witnesses {
            let WitnessKind::DirectCapability {
                entry_id, entity, ..
            } = &w.kind
            else {
                continue;
            };
            let key = (entry_id.clone(), entity.clone());
            if w.seed_class.is_ambient() {
                idx.ambient_entities.insert(key.clone());
                idx.ambient_owners.insert(w.owner_candidate_id.clone());
            }
            if w.seed_class.is_primary() {
                idx.primary_entities.insert(key);
                idx.primary_owners.insert(w.owner_candidate_id.clone());
            }
        }
        idx
    }

    #[inline]
    pub fn entity_is_ambient(&self, entry_id: &str, entity: &str) -> bool {
        self.ambient_entities
            .iter()
            .any(|(e, ent)| e == entry_id && ent == entity)
    }

    #[inline]
    pub fn entity_is_primary(&self, entry_id: &str, entity: &str) -> bool {
        self.primary_entities
            .iter()
            .any(|(e, ent)| e == entry_id && ent == entity)
    }

    #[inline]
    pub fn owner_is_ambient(&self, owner_candidate_id: &str) -> bool {
        self.ambient_owners.contains(owner_candidate_id)
    }

    #[inline]
    pub fn owner_is_primary(&self, owner_candidate_id: &str) -> bool {
        self.primary_owners.contains(owner_candidate_id)
    }
}
