//! Append-only symbol assignment during exposure waves.

use indexmap::IndexMap;

use crate::identity::{CapabilityParamName, EntityFieldName, RegistryEntryId, RelationName};
use crate::schema::{capability_path_method_segment, CGS};
use crate::CapabilityKind;

use super::keys::{
    CapParamKey, EntityFieldKey, MethodKey, MethodSegmentKey, OpaqueMSym, OpaquePSym, OpaqueRSym,
    OpaqueVSym, RelationKey,
};
use super::slot_meta_is_relation;
use super::{
    collect_slot_metas_for_surface, slot_occurrence_key, slot_symbol_allocation_fingerprint,
    IdentMetadata, IdentRole, TeachingExposureSession,
};

fn prefer_entity_field_representative(
    existing: &IdentMetadata,
    incoming: &IdentMetadata,
) -> IdentMetadata {
    if matches!(
        existing.allocation_ident_role(),
        IdentRole::CapabilityParam { .. }
    ) && matches!(incoming.allocation_ident_role(), IdentRole::EntityField)
    {
        incoming.clone()
    } else {
        existing.clone()
    }
}

impl TeachingExposureSession {
    pub(super) fn assign_new_methods_and_idents(&mut self, cgs_layers: &[&CGS]) {
        let _ = cgs_layers;
        let mut new_keys: Vec<MethodKey> = Vec::new();
        for cap_key in self.surface.capabilities.iter() {
            let Some(cgs) = self.catalog_cgs.get(&cap_key.entry_id) else {
                continue;
            };
            let Some(cap) = cgs.capabilities.get(&cap_key.capability) else {
                continue;
            };
            let key = MethodKey::new(
                RegistryEntryId::from(cap_key.entry_id.as_str()),
                cap.domain.clone(),
                cap.name.clone(),
            );
            if !self.tables.method_to_sym.contains_key(&key) {
                new_keys.push(key);
            }
        }
        new_keys.sort();
        for (next_m, key) in (self.tables.sym_to_method.len() + 1..).zip(new_keys) {
            let sym = OpaqueMSym::from_zero_based((next_m - 1) as u32);
            self.tables.method_to_sym.insert(key.clone(), sym);
            let segment = MethodSegmentKey {
                entry_id: key.entry_id.clone(),
                domain: key.domain.clone(),
                segment: capability_path_method_segment(
                    self.catalog_cgs
                        .get(key.entry_id.as_str())
                        .and_then(|cgs| cgs.capabilities.get(key.capability.as_str()))
                        .expect("cap"),
                ),
            };
            self.tables.method_segment_to_sym.insert(segment, sym);
            let kind = self
                .catalog_cgs
                .get(key.entry_id.as_str())
                .and_then(|cgs| cgs.capabilities.get(key.capability.as_str()))
                .map(|cap| cap.kind)
                .unwrap_or(CapabilityKind::Action);
            self.record_method_binding(
                sym,
                key.entry_id.clone(),
                key.domain.clone(),
                key.capability.clone(),
                kind,
            );
        }

        self.assign_new_slot_symbols();
    }

    pub(super) fn assign_new_slot_symbols(&mut self) {
        let mut collected: Vec<IdentMetadata> =
            collect_slot_metas_for_surface(&self.catalog_cgs, &self.surface);
        collected.sort_by(|a, b| {
            slot_symbol_allocation_fingerprint(a).cmp(&slot_symbol_allocation_fingerprint(b))
        });
        let mut by_fp: IndexMap<String, IdentMetadata> = IndexMap::new();
        let mut new_occurrence_keys: Vec<String> = Vec::new();
        for m in &collected {
            let occ_key = slot_occurrence_key(m);
            if !self.ledger.slot_occurrence_meta.contains_key(&occ_key) {
                new_occurrence_keys.push(occ_key.clone());
            }
            self.ledger
                .slot_occurrence_meta
                .entry(occ_key)
                .or_insert_with(|| m.clone());
            let fp = slot_symbol_allocation_fingerprint(m);
            by_fp
                .entry(fp)
                .and_modify(|existing| {
                    *existing = prefer_entity_field_representative(existing, m);
                })
                .or_insert_with(|| m.clone());
        }
        for (fp, meta) in &by_fp {
            self.ledger
                .fingerprint_meta
                .entry(fp.clone())
                .and_modify(|existing| {
                    *existing = prefer_entity_field_representative(existing, meta);
                })
                .or_insert_with(|| meta.clone());
        }

        let mut value_fps_in_wave: IndexMap<String, IdentMetadata> = IndexMap::new();
        for meta in by_fp.values() {
            if let Some(vfp) = meta.value_domain_allocation_fp() {
                value_fps_in_wave
                    .entry(vfp)
                    .or_insert_with(|| (*meta).clone());
            }
        }
        let mut new_v_fps: Vec<String> = value_fps_in_wave
            .keys()
            .filter(|fp| !self.ledger.value_domain_fp_to_sym.contains_key(*fp))
            .cloned()
            .collect();
        new_v_fps.sort();
        let base_v = self.ledger.value_domain_fp_to_sym.len();
        for (i, fp) in new_v_fps.iter().enumerate() {
            let sym = OpaqueVSym::from_zero_based((base_v + i) as u32);
            self.ledger.value_domain_fp_to_sym.insert(fp.clone(), sym);
            self.ledger
                .value_domain_fp_to_repr_meta
                .entry(fp.clone())
                .or_insert_with(|| value_fps_in_wave.get(fp).expect("vfp").clone());
        }

        let mut new_p_fps: Vec<String> = by_fp
            .keys()
            .filter(|fp| {
                self.ledger
                    .fingerprint_meta
                    .get(*fp)
                    .is_some_and(|m| !slot_meta_is_relation(m))
                    && !self.ledger.slot_fingerprint_to_sym.contains_key(*fp)
            })
            .cloned()
            .collect();
        new_p_fps.sort();
        for (next_p, fp) in (self.ledger.slot_fingerprint_to_sym.len() + 1..).zip(new_p_fps.iter())
        {
            let sym = OpaquePSym::from_zero_based((next_p - 1) as u32);
            self.ledger.slot_fingerprint_to_sym.insert(fp.clone(), sym);
            self.commit_slot_binding_for_fp(fp);
        }

        let mut new_r_fps: Vec<String> = by_fp
            .keys()
            .filter(|fp| {
                self.ledger
                    .fingerprint_meta
                    .get(*fp)
                    .is_some_and(slot_meta_is_relation)
                    && !self.ledger.relation_fingerprint_to_sym.contains_key(*fp)
            })
            .cloned()
            .collect();
        new_r_fps.sort();
        for (next_r, fp) in
            (self.ledger.relation_fingerprint_to_sym.len() + 1..).zip(new_r_fps.iter())
        {
            let sym = OpaqueRSym::from_zero_based((next_r - 1) as u32);
            self.ledger
                .relation_fingerprint_to_sym
                .insert(fp.clone(), sym);
            self.record_relation_binding_for_fp(fp);
        }
        self.commit_slot_maps_for_wave(&new_p_fps, &new_r_fps, &new_occurrence_keys);
        self.append_ident_meta_for_wave(&by_fp);
    }

    fn append_ident_meta_for_wave(&mut self, _by_fp: &IndexMap<String, IdentMetadata>) {
        for meta in self.ledger.slot_occurrence_meta.values() {
            let key = (meta.catalog_entry_id().to_string(), meta.entity().clone());
            self.ident_meta_by_entity
                .entry(key)
                .or_default()
                .entry(meta.wire_name().to_string())
                .or_insert_with(|| meta.clone());
        }
    }

    fn commit_slot_maps_for_wave(
        &mut self,
        new_p_fps: &[String],
        new_r_fps: &[String],
        new_occurrence_keys: &[String],
    ) {
        for fp in new_p_fps {
            let Some(meta) = self.ledger.fingerprint_meta.get(fp).cloned() else {
                continue;
            };
            if slot_meta_is_relation(&meta) {
                continue;
            }
            let Some(sym) = self.ledger.slot_fingerprint_to_sym.get(fp).copied() else {
                continue;
            };
            self.commit_slot_forward_maps(&meta, sym);
        }
        for fp in new_r_fps {
            let Some(meta) = self.ledger.fingerprint_meta.get(fp).cloned() else {
                continue;
            };
            let Some(sym) = self.ledger.relation_fingerprint_to_sym.get(fp).copied() else {
                continue;
            };
            self.tables.relation_to_sym.insert(
                RelationKey::new(
                    RegistryEntryId::from(meta.catalog_entry_id()),
                    meta.entity().clone(),
                    RelationName::from(meta.wire_name()),
                ),
                sym,
            );
        }
        for occ_key in new_occurrence_keys {
            let Some(meta) = self.ledger.slot_occurrence_meta.get(occ_key).cloned() else {
                continue;
            };
            let fp = slot_symbol_allocation_fingerprint(&meta);
            if slot_meta_is_relation(&meta) {
                let Some(sym) = self.ledger.relation_fingerprint_to_sym.get(&fp).copied() else {
                    continue;
                };
                self.tables.relation_to_sym.insert(
                    RelationKey::new(
                        RegistryEntryId::from(meta.catalog_entry_id()),
                        meta.entity().clone(),
                        RelationName::from(meta.wire_name()),
                    ),
                    sym,
                );
                continue;
            }
            let Some(sym) = self.ledger.slot_fingerprint_to_sym.get(&fp).copied() else {
                continue;
            };
            self.commit_slot_forward_maps(&meta, sym);
        }
    }

    fn commit_slot_forward_maps(&mut self, meta: &IdentMetadata, sym: OpaquePSym) {
        match meta.allocation_ident_role() {
            IdentRole::EntityField => {
                self.tables.entity_field_to_sym.insert(
                    EntityFieldKey::new(
                        RegistryEntryId::from(meta.catalog_entry_id()),
                        meta.entity().clone(),
                        EntityFieldName::from(meta.wire_name()),
                    ),
                    sym,
                );
            }
            IdentRole::Relation { .. } => {}
            IdentRole::CapabilityParam { capability } => {
                let key = CapParamKey::new(
                    RegistryEntryId::from(meta.catalog_entry_id()),
                    meta.entity().clone(),
                    capability.clone(),
                    CapabilityParamName::from(meta.wire_name()),
                );
                self.tables.cap_param_to_sym.insert(key.clone(), sym);
                self.tables.sym_to_cap_param_key.insert(sym, key);
            }
        }
    }
}
