//! Insert decoded entity trees into the session graph (CEP-10 bounded embed depth).

use plasm_compile::{flatten_decoded_embed_descendants, DecodedEntity};

use crate::materialization::SessionMaterialization;
use crate::{CachedEntity, EntityCompleteness, RuntimeError};

pub(crate) fn cache_decoded_entity_tree(
    mat: &mut SessionMaterialization,
    decoded: DecodedEntity,
    timestamp: u64,
    completeness: EntityCompleteness,
) -> Result<CachedEntity, RuntimeError> {
    for embedded in flatten_decoded_embed_descendants(&decoded) {
        if embedded.reference == decoded.reference {
            continue;
        }
        let child = CachedEntity::from_decoded(
            embedded.reference,
            embedded.fields,
            embedded.relations,
            timestamp,
            // A parent response does not prove the child's GET projection is
            // complete; embeds may contain only an identity or list summary.
            EntityCompleteness::Summary,
        );
        if mat.allows_recorded_read_reuse() {
            mat.insert(child)?;
        } else {
            mat.publish_fresh_row(child)?;
        }
    }
    let cached = CachedEntity::from_decoded(
        decoded.reference,
        decoded.fields,
        decoded.relations,
        timestamp,
        completeness,
    );
    if mat.allows_recorded_read_reuse() {
        mat.insert(cached.clone())?;
    } else {
        mat.publish_fresh_row(cached.clone())?;
    }
    Ok(cached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_compile::DecodedRelation;
    use plasm_core::Ref;
    use proptest::prelude::*;

    fn observation(ids: &[u8]) -> DecodedEntity {
        DecodedEntity {
            reference: Ref::new("Collection", "one"),
            fields: Default::default(),
            relations: [(
                "items".into(),
                DecodedRelation::Specified(
                    ids.iter()
                        .map(|id| Ref::new("Item", id.to_string()))
                        .collect(),
                ),
            )]
            .into(),
            embedded_entities: Vec::new(),
            field_diagnostics: Vec::new(),
        }
    }

    proptest! {
        #[test]
        fn post_mutation_get_membership_survives_read_branch_absorb(
            old in proptest::collection::vec(0u8..24, 0..24),
            new in proptest::collection::vec(0u8..24, 0..24),
            fresh_first in any::<bool>(),
        ) {
            let mut parent = SessionMaterialization::new();
            cache_decoded_entity_tree(&mut parent, observation(&old), 0, EntityCompleteness::Complete).unwrap();
            parent.poison_read_caches_after_mutation();
            let (mut fresh, _) = crate::BranchMaterializationBase::fork_from(&parent);
            let (mut sibling, _) = crate::BranchMaterializationBase::fork_from(&parent);
            let expected = cache_decoded_entity_tree(&mut fresh, observation(&new), 1, EntityCompleteness::Complete).unwrap();
            prop_assert_eq!(&fresh.get(&expected.reference).unwrap().relations, &expected.relations);
            sibling.insert(CachedEntity::from_decoded(
                Ref::new("Other", "one"), Default::default(), Default::default(), 1, EntityCompleteness::Complete,
            )).unwrap();
            if fresh_first {
                parent.absorb_branch(fresh).unwrap();
                parent.absorb_branch(sibling).unwrap();
            } else {
                parent.absorb_branch(sibling).unwrap();
                parent.absorb_branch(fresh).unwrap();
            }
            prop_assert_eq!(&parent.get(&expected.reference).unwrap().relations, &expected.relations);
            prop_assert!(parent.get(&Ref::new("Other", "one")).is_some());
        }
    }
}
