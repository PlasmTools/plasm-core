//! Exposure-surface gates for incremental teaching waves.

use crate::identity::{EntityFieldName, EntityName, RelationName};
use crate::schema::CGS;
use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
};

#[inline]
pub(crate) fn surface_allows_capability(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    cap: &crate::schema::CapabilitySchema,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    s.capabilities.contains(&ExposureCapabilityKey {
        entry_id: catalog_entry_id.to_string(),
        domain: cap.domain.clone(),
        capability: cap.name.clone(),
    })
}

#[inline]
pub(crate) fn surface_allows_entity_field(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    entity: &str,
    field: &str,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    let ekey = ExposureEntityKey {
        entry_id: catalog_entry_id.to_string(),
        entity: EntityName::from(entity),
    };
    s.slots.contains(&ExposureSlotKey::EntityField {
        entity: ekey,
        field: EntityFieldName::new(field.to_string()),
    })
}

#[inline]
pub(crate) fn surface_allows_relation_nav(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    entity: &str,
    relation: &str,
    is_declared_relation: bool,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    if is_declared_relation {
        let source = ExposureEntityKey {
            entry_id: catalog_entry_id.to_string(),
            entity: EntityName::from(entity),
        };
        return s.slots.contains(&ExposureSlotKey::Relation {
            source,
            relation: RelationName::new(relation.to_string()),
        });
    }
    surface_allows_entity_field(surface, catalog_entry_id, entity, relation)
}

/// Canonical catalog-qualified entity key for [`ExposureSurface::entities`] membership checks.
pub(crate) fn exposure_entity_key_for_surface(
    cgs: &CGS,
    catalog_entry_id: &str,
    raw_entity: &str,
) -> Option<ExposureEntityKey> {
    let raw = raw_entity.trim();
    if raw.is_empty() {
        return None;
    }
    for k in cgs.entities.keys() {
        if k.eq_ignore_ascii_case(raw) {
            return Some(ExposureEntityKey {
                entry_id: catalog_entry_id.to_string(),
                entity: EntityName::from(k.as_str()),
            });
        }
    }
    None
}

/// Catalog-qualified entity appears in [`ExposureSurface::entities`] (canonical name via CGS keys).
/// Without a surface (`None`), treated as included (legacy full teaching table).
#[inline]
pub(crate) fn surface_includes_exposed_entity(
    surface: Option<&ExposureSurface>,
    cgs: &CGS,
    catalog_entry_id: &str,
    raw_entity: &str,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    let Some(ekey) = exposure_entity_key_for_surface(cgs, catalog_entry_id, raw_entity) else {
        return false;
    };
    s.entities.contains(&ekey)
}

/// Relation-navigation rows (`… .r#` or wire toward another CGS entity, or declared relation chains) are only
/// taught when the **target** entity name appears in [`ExposureSurface::entities`] for the same
/// `catalog_entry_id`. Without a surface (`None`), navigation is unrestricted (legacy full teaching table).
#[inline]
pub(crate) fn surface_exposes_relation_nav_target(
    surface: Option<&ExposureSurface>,
    cgs: &CGS,
    catalog_entry_id: &str,
    target_entity: &str,
) -> bool {
    surface_includes_exposed_entity(surface, cgs, catalog_entry_id, target_entity)
}
