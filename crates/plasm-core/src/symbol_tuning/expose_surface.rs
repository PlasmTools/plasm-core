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
        crate::capability_exposure::admit_exposed_relations(
            owning_cgs.as_ref(),
            catalog_entry_id,
            &mut self.surface,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_exposure::selected_capability_surface,
        prompt_render::python::{prepare_python_teaching_wave, PythonTeachingState},
    };

    #[test]
    fn split_endpoint_waves_teach_the_same_relation_as_joint_exposure() {
        let cgs = Arc::new(
            crate::loader::load_schema_dir(
                &std::path::Path::new(crate::test_manifest_dir())
                    .join("../../fixtures/schemas/from_parent_get_nav"),
            )
            .unwrap(),
        );
        let names = ["parent_item_get", "tag_query"];
        let select = |names: &[&str]| {
            selected_capability_surface(
                &cgs,
                "matrix",
                &names.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let joint = TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "matrix",
            &["ParentItem", "Tag"],
            select(&names),
        );
        assert_eq!(joint.exposed_relation_symbol_rows().len(), 1);
        for (first, second, first_entity, second_entity) in [
            (names[0], names[1], "ParentItem", "Tag"),
            (names[1], names[0], "Tag", "ParentItem"),
        ] {
            let mut split = TeachingExposureSession::new_with_intent_delta(
                &cgs,
                "matrix",
                &[first_entity],
                select(&[first]),
            );
            assert!(split.exposed_relation_symbol_rows().is_empty());
            let before =
                prepare_python_teaching_wave(&split, &PythonTeachingState::default()).unwrap();
            let original_entities = split.to_symbol_map().exposed_entity_symbol_rows();
            split.expose_surface(
                &[&cgs],
                cgs.clone(),
                "matrix",
                &[second_entity],
                select(&[second]),
            );
            assert_eq!(split.surface, joint.surface, "{first} then {second}");
            let relations = split.exposed_relation_symbol_rows();
            assert_eq!(relations.len(), 1);
            let after = prepare_python_teaching_wave(&split, &before.next_state).unwrap();
            assert!(after.declarations.contains(&relations[0].symbol));
            for original in original_entities {
                assert!(split
                    .to_symbol_map()
                    .exposed_entity_symbol_rows()
                    .iter()
                    .any(|e| e.entry_id == original.entry_id
                        && e.entity == original.entity
                        && e.symbol == original.symbol));
            }
            let stable = split.exposed_relation_symbol_rows();
            split.expose_surface(
                &[&cgs],
                cgs.clone(),
                "matrix",
                &[second_entity],
                select(&[second]),
            );
            assert_eq!(split.exposed_relation_symbol_rows(), stable);
            assert_eq!(split.surface.capabilities.len(), 2);
        }
    }

    #[test]
    fn relation_endpoints_in_different_catalogs_do_not_join() {
        let cgs = Arc::new(
            crate::loader::load_schema_dir(
                &std::path::Path::new(crate::test_manifest_dir())
                    .join("../../fixtures/schemas/from_parent_get_nav"),
            )
            .unwrap(),
        );
        let first = selected_capability_surface(&cgs, "left", &["parent_item_get".into()]).unwrap();
        let mut exposure =
            TeachingExposureSession::new_with_intent_delta(&cgs, "left", &["ParentItem"], first);
        let second = selected_capability_surface(&cgs, "right", &["tag_query".into()]).unwrap();
        exposure.expose_surface(&[&cgs], cgs.clone(), "right", &["Tag"], second);
        assert!(exposure.exposed_relation_symbol_rows().is_empty());
        assert_eq!(exposure.surface.capabilities.len(), 2);
    }
}
