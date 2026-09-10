//! Exact teaching exposure for selected business and prerequisite capabilities.

use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
    ExposureSurfaceDelta,
};
use crate::CGS;
use std::collections::BTreeSet;

/// Project selected IDs without intent scoring, implicit parent promotion or sibling admission.
pub fn selected_capability_surface(
    cgs: &CGS,
    entry_id: &str,
    capabilities: &[String],
) -> Result<ExposureSurfaceDelta, String> {
    let mut surface = ExposureSurface::default();
    for name in capabilities {
        let cap = cgs
            .capabilities
            .get(name.as_str())
            .ok_or_else(|| format!("unknown selected capability {entry_id}/{name}"))?;
        let entity = cgs
            .entities
            .get(cap.domain.as_str())
            .ok_or("selected capability entity missing")?;
        let entity_key = ExposureEntityKey {
            entry_id: entry_id.into(),
            entity: cap.domain.clone(),
        };
        surface.entities.insert(entity_key.clone());
        let capability_key = ExposureCapabilityKey {
            entry_id: entry_id.into(),
            domain: cap.domain.clone(),
            capability: cap.name.clone(),
        };
        surface.capabilities.insert(capability_key.clone());
        for field in cap.input_fields() {
            surface.slots.insert(ExposureSlotKey::CapabilityParam {
                capability: capability_key.clone(),
                param: field.name.clone().into(),
            });
        }
        surface.slots.insert(ExposureSlotKey::EntityField {
            entity: entity_key.clone(),
            field: entity.id_field.clone(),
        });
        for field in &cap.provides {
            surface.slots.insert(ExposureSlotKey::EntityField {
                entity: entity_key.clone(),
                field: field.clone().into(),
            });
        }
        if let Some(output) = &cap.output_schema {
            if let crate::schema::OutputType::Entity { entity_type }
            | crate::schema::OutputType::Collection { entity_type, .. } = &output.output_type
            {
                let output_entity = cgs
                    .entities
                    .get(entity_type.as_str())
                    .ok_or("selected output entity missing")?;
                let output_key = ExposureEntityKey {
                    entry_id: entry_id.into(),
                    entity: entity_type.as_str().into(),
                };
                surface.entities.insert(output_key.clone());
                for field in output_entity.fields.keys() {
                    if cap.provides.is_empty()
                        || cap.provides.iter().any(|name| name == field.as_str())
                    {
                        surface.slots.insert(ExposureSlotKey::EntityField {
                            entity: output_key.clone(),
                            field: field.clone(),
                        });
                    }
                }
            }
        }
    }
    let entities: BTreeSet<_> = surface.entities.iter().cloned().collect();
    for key in &entities {
        let entity = cgs
            .entities
            .get(key.entity.as_str())
            .ok_or("exposed entity missing")?;
        for (name, relation) in &entity.relations {
            if entities.contains(&ExposureEntityKey {
                entry_id: entry_id.into(),
                entity: relation.target_resource.clone(),
            }) {
                surface.slots.insert(ExposureSlotKey::Relation {
                    source: key.clone(),
                    relation: name.clone(),
                });
            }
        }
    }
    Ok(ExposureSurfaceDelta { required: surface })
}

/// Explicit execution exposes capabilities owned by the caller-selected entities.
/// Intent routing must pass selected capability IDs to `selected_capability_surface` instead.
pub fn explicit_entity_capability_surface(
    cgs: &CGS,
    entry: &str,
    entities: &[String],
) -> Result<ExposureSurfaceDelta, String> {
    let capabilities = cgs
        .capabilities
        .values()
        .filter(|capability| {
            entities
                .iter()
                .any(|entity| entity == capability.domain.as_str())
        })
        .map(|capability| capability.name.to_string())
        .collect::<Vec<_>>();
    selected_capability_surface(cgs, entry, &capabilities)
}
