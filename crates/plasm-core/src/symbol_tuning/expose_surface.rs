//! Entity admission and bare-entity pruning for [`TeachingExposureSession::expose_surface`].

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::identity::EntityName;
use crate::schema::CGS;

use super::keys::{OpaqueESym, QualifiedEntityKey};
use super::{
    ExposureAppendReport, ExposureEntityKey, ExposureSurface, ExposureSurfaceDelta,
    TeachingExposureSession,
};

/// Drop bare entity keys with zero capabilities (relation tips with no teaches).
/// Seeds always survive even without capability rows.
fn prune_bare_surface_entities_without_capabilities(
    surface: &mut ExposureSurface,
    catalog_entry_id: &str,
    seed_names: &BTreeSet<&str>,
) {
    surface.entities.retain(|ek| {
        if ek.entry_id != catalog_entry_id {
            return true;
        }
        if seed_names.contains(ek.entity.as_str()) {
            return true;
        }
        surface
            .capabilities
            .iter()
            .any(|c| c.entry_id == ek.entry_id && c.domain.as_str() == ek.entity.as_str())
    });
}

/// Admit an entity onto the session `e#` ledger when it is on the surface and not yet assigned.
fn assign_surface_entity_e(
    session: &mut TeachingExposureSession,
    owning_cgs: &CGS,
    catalog_entry_id: &str,
    ename: &str,
) -> bool {
    let ekey = ExposureEntityKey {
        entry_id: catalog_entry_id.to_string(),
        entity: EntityName::from(ename),
    };
    if !session.surface.entities.contains(&ekey) {
        return false;
    }
    let qkey = QualifiedEntityKey::new(catalog_entry_id, ename);
    if session.tables.qualified_entity_to_sym.contains_key(&qkey) {
        return false;
    }
    if owning_cgs.get_entity(ename).is_none() {
        return false;
    }
    let sym = OpaqueESym::from_zero_based(session.entities.len() as u32);
    session.entities.push(ename.to_string());
    session
        .entity_catalog_entry_ids
        .push(catalog_entry_id.to_string());
    session.tables.qualified_entity_to_sym.insert(qkey, sym);
    session.record_entity_binding(sym, catalog_entry_id, ename);
    true
}

/// Capability-bearing surface entities for `catalog_entry_id`, sorted and deduped.
fn capability_bearing_extra_entities(
    surface: &ExposureSurface,
    catalog_entry_id: &str,
) -> Vec<String> {
    let mut extras: Vec<String> = surface
        .entities
        .iter()
        .filter(|ek| ek.entry_id == catalog_entry_id)
        .filter(|ek| {
            surface
                .capabilities
                .iter()
                .any(|c| c.entry_id == ek.entry_id && c.domain.as_str() == ek.entity.as_str())
        })
        .map(|ek| ek.entity.to_string())
        .collect();
    extras.sort();
    extras.dedup();
    extras
}

impl TeachingExposureSession {
    pub fn expose_surface(
        &mut self,
        cgs_layers: &[&CGS],
        owning_cgs: Arc<CGS>,
        catalog_entry_id: &str,
        entity_names_in_order: &[&str],
        delta: ExposureSurfaceDelta,
    ) -> ExposureAppendReport {
        if cgs_layers.is_empty() {
            return ExposureAppendReport::default();
        }
        self.catalog_cgs
            .insert(catalog_entry_id.to_string(), owning_cgs.clone());
        self.ledger.clear_symbol_map_cache();
        self.surface.merge_from(&delta.required);
        let seed_names: BTreeSet<&str> = entity_names_in_order.iter().copied().collect();
        prune_bare_surface_entities_without_capabilities(
            &mut self.surface,
            catalog_entry_id,
            &seed_names,
        );
        let mut entities_added = 0usize;
        for n in entity_names_in_order {
            if assign_surface_entity_e(self, owning_cgs.as_ref(), catalog_entry_id, n) {
                entities_added += 1;
            }
        }
        // Relation-target mutator closure may expose entities with capabilities; give them e#.
        for ename in capability_bearing_extra_entities(&self.surface, catalog_entry_id) {
            if assign_surface_entity_e(self, owning_cgs.as_ref(), catalog_entry_id, ename.as_str())
            {
                entities_added += 1;
            }
        }
        self.assign_new_methods_and_idents(cgs_layers);
        ExposureAppendReport { entities_added }
    }
}
