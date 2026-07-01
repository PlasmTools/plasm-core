//! Postcard-native wire shapes for [`super::IdentMetadata`] (no YAML singleton maps / JSON nesting).

use serde::{Deserialize, Serialize};

use super::IdentRegistryRole;
use crate::identity::{CapabilityName, EntityName};
use crate::schema::{ArrayItemsSchema, FieldValueKind, StringSemantics, ValueDomainKey};
use crate::FieldType;
use crate::ValueWireFormat;

use super::IdentMetadata;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedArrayItemsSchema {
    pub value_ref: String,
    pub field_type: FieldType,
    pub value_format: Option<ValueWireFormat>,
    pub allowed_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedIdentRegistryRole {
    EntityField,
    CapabilityParam { capability: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PersistedIdentMetadata {
    RegistryBacked {
        catalog_entry_id: String,
        entity: String,
        role: PersistedIdentRegistryRole,
        value_registry_key: String,
        field_type: FieldType,
        string_semantics: Option<StringSemantics>,
        array_items: Option<PersistedArrayItemsSchema>,
        allowed_values: Option<Vec<String>>,
        wire_name: String,
        description: String,
    },
    Relation {
        catalog_entry_id: String,
        entity: String,
        wire_name: String,
        description: String,
        target: String,
    },
    SyntheticUnknown {
        catalog_entry_id: String,
        entity: String,
        wire_name: String,
        description: String,
    },
    CapabilityStructuralSlot {
        catalog_entry_id: String,
        entity: String,
        capability: String,
        param_path: String,
        description: String,
    },
}

impl PersistedArrayItemsSchema {
    fn from_schema(items: &ArrayItemsSchema) -> Self {
        let value_ref = match &items.kind {
            FieldValueKind::Registry(k) => k.as_str().to_string(),
        };
        Self {
            value_ref,
            field_type: items.field_type.clone(),
            value_format: items.value_format,
            allowed_values: items.allowed_values.clone(),
        }
    }

    fn into_schema(self) -> Result<ArrayItemsSchema, String> {
        let kind = FieldValueKind::Registry(ValueDomainKey::new(self.value_ref)?);
        Ok(ArrayItemsSchema {
            kind,
            field_type: self.field_type,
            value_format: self.value_format,
            allowed_values: self.allowed_values,
        })
    }
}

impl From<&IdentMetadata> for PersistedIdentMetadata {
    fn from(meta: &IdentMetadata) -> Self {
        match meta {
            IdentMetadata::RegistryBacked {
                catalog_entry_id,
                entity,
                role,
                value_registry_key,
                field_type,
                string_semantics,
                array_items,
                allowed_values,
                wire_name,
                description,
            } => Self::RegistryBacked {
                catalog_entry_id: catalog_entry_id.clone(),
                entity: entity.as_str().to_string(),
                role: match role {
                    IdentRegistryRole::EntityField => PersistedIdentRegistryRole::EntityField,
                    IdentRegistryRole::CapabilityParam { capability } => {
                        PersistedIdentRegistryRole::CapabilityParam {
                            capability: capability.as_str().to_string(),
                        }
                    }
                },
                value_registry_key: value_registry_key.as_str().to_string(),
                field_type: field_type.clone(),
                string_semantics: *string_semantics,
                array_items: array_items
                    .as_ref()
                    .map(PersistedArrayItemsSchema::from_schema),
                allowed_values: allowed_values.clone(),
                wire_name: wire_name.clone(),
                description: description.clone(),
            },
            IdentMetadata::Relation {
                catalog_entry_id,
                entity,
                wire_name,
                description,
                target,
            } => Self::Relation {
                catalog_entry_id: catalog_entry_id.clone(),
                entity: entity.as_str().to_string(),
                wire_name: wire_name.clone(),
                description: description.clone(),
                target: target.as_str().to_string(),
            },
            IdentMetadata::SyntheticUnknown {
                catalog_entry_id,
                entity,
                wire_name,
                description,
            } => Self::SyntheticUnknown {
                catalog_entry_id: catalog_entry_id.clone(),
                entity: entity.as_str().to_string(),
                wire_name: wire_name.clone(),
                description: description.clone(),
            },
            IdentMetadata::CapabilityStructuralSlot {
                catalog_entry_id,
                entity,
                capability,
                param_path,
                description,
            } => Self::CapabilityStructuralSlot {
                catalog_entry_id: catalog_entry_id.clone(),
                entity: entity.as_str().to_string(),
                capability: capability.as_str().to_string(),
                param_path: param_path.clone(),
                description: description.clone(),
            },
        }
    }
}

impl PersistedIdentMetadata {
    pub fn into_ident_metadata(self) -> Result<IdentMetadata, String> {
        Ok(match self {
            Self::RegistryBacked {
                catalog_entry_id,
                entity,
                role,
                value_registry_key,
                field_type,
                string_semantics,
                array_items,
                allowed_values,
                wire_name,
                description,
            } => IdentMetadata::RegistryBacked {
                catalog_entry_id,
                entity: EntityName::from(entity),
                role: match role {
                    PersistedIdentRegistryRole::EntityField => IdentRegistryRole::EntityField,
                    PersistedIdentRegistryRole::CapabilityParam { capability } => {
                        IdentRegistryRole::CapabilityParam {
                            capability: CapabilityName::from(capability),
                        }
                    }
                },
                value_registry_key: ValueDomainKey::new(value_registry_key)?,
                field_type,
                string_semantics,
                array_items: array_items
                    .map(PersistedArrayItemsSchema::into_schema)
                    .transpose()?,
                allowed_values,
                wire_name,
                description,
            },
            Self::Relation {
                catalog_entry_id,
                entity,
                wire_name,
                description,
                target,
            } => IdentMetadata::Relation {
                catalog_entry_id,
                entity: EntityName::from(entity),
                wire_name,
                description,
                target: EntityName::from(target),
            },
            Self::SyntheticUnknown {
                catalog_entry_id,
                entity,
                wire_name,
                description,
            } => IdentMetadata::SyntheticUnknown {
                catalog_entry_id,
                entity: EntityName::from(entity),
                wire_name,
                description,
            },
            Self::CapabilityStructuralSlot {
                catalog_entry_id,
                entity,
                capability,
                param_path,
                description,
            } => IdentMetadata::CapabilityStructuralSlot {
                catalog_entry_id,
                entity: EntityName::from(entity),
                capability: CapabilityName::from(capability),
                param_path,
                description,
            },
        })
    }
}
