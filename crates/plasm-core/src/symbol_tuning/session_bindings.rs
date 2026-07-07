//! All forward rows are written during [`TeachingExposureSession`] symbol assignment
//! (`expose_entities`, method waves, [`assign_new_slot_symbols`]) — not recomputed at snapshot time.

use crate::identity::{CapabilityName, EntityName, RegistryEntryId, RelationName};
use crate::CapabilityKind;

use super::keys::{OpaqueESym, OpaqueMSym, OpaqueRSym};
use super::{IdentMetadata, TeachingExposureSession};

/// Owning catalog + entity for a session `e#` token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntityBinding {
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
}

impl EntityBinding {
    pub fn entry_id_str(&self) -> &str {
        self.entry_id.as_str()
    }

    pub fn entity_str(&self) -> &str {
        self.entity.as_str()
    }
}

/// Owning catalog + domain + capability wire for a session `m#` token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MethodBinding {
    pub entry_id: RegistryEntryId,
    pub domain: EntityName,
    pub capability: CapabilityName,
    pub kind: CapabilityKind,
}

impl MethodBinding {
    pub fn entry_id_str(&self) -> &str {
        self.entry_id.as_str()
    }

    pub fn domain_str(&self) -> &str {
        self.domain.as_str()
    }

    pub fn capability_str(&self) -> &str {
        self.capability.as_str()
    }
}

/// Declared relation hop for a session `r#` token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelationBinding {
    pub entry_id: RegistryEntryId,
    pub source_entity: EntityName,
    pub relation_wire: RelationName,
    pub target_entity: EntityName,
}

impl RelationBinding {
    pub fn relation_wire_str(&self) -> &str {
        self.relation_wire.as_str()
    }
}

pub(crate) fn relation_binding_from_meta(meta: &IdentMetadata) -> Option<RelationBinding> {
    let IdentMetadata::Relation {
        catalog_entry_id,
        entity,
        wire_name,
        target,
        ..
    } = meta
    else {
        return None;
    };
    Some(RelationBinding {
        entry_id: RegistryEntryId::from(catalog_entry_id.as_str()),
        source_entity: entity.clone(),
        relation_wire: RelationName::from(wire_name.as_str()),
        target_entity: target.clone(),
    })
}

impl TeachingExposureSession {
    pub(crate) fn record_entity_binding(&mut self, sym: OpaqueESym, entry_id: &str, entity: &str) {
        self.tables.sym_to_entity_binding.insert(
            sym,
            EntityBinding {
                entry_id: RegistryEntryId::from(entry_id),
                entity: EntityName::from(entity),
            },
        );
    }

    pub(crate) fn record_method_binding(
        &mut self,
        sym: OpaqueMSym,
        entry_id: RegistryEntryId,
        domain: EntityName,
        capability: CapabilityName,
        kind: CapabilityKind,
    ) {
        let binding = MethodBinding {
            entry_id: entry_id.clone(),
            domain: domain.clone(),
            capability: capability.clone(),
            kind,
        };
        self.tables.sym_to_method.insert(sym, binding);
    }

    pub(crate) fn record_relation_binding(&mut self, sym: OpaqueRSym, meta: &IdentMetadata) {
        if let Some(binding) = relation_binding_from_meta(meta) {
            self.tables.sym_to_relation_binding.insert(sym, binding);
        }
    }
}
