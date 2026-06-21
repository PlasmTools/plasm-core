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
            EntityCompleteness::Complete,
        );
        mat.insert(child)?;
    }
    let cached = CachedEntity::from_decoded(
        decoded.reference,
        decoded.fields,
        decoded.relations,
        timestamp,
        completeness,
    );
    mat.insert(cached.clone())?;
    Ok(cached)
}
