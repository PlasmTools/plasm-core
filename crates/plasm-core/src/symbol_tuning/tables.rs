//! Shared resolve/render map bundle and session-only assignment ledger.

use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::schema::CGS;

use super::keys::{
    CapParamKey, EntityFieldKey, MethodKey, MethodSegmentKey, OpaqueESym, OpaqueMSym, OpaquePSym,
    OpaqueRSym, OpaqueVSym, QualifiedEntityKey, RelationKey,
};
use super::session_bindings::{
    EntityBinding, MethodBinding, RelationBinding, SlotBinding,
};
use super::{IdentMetadata, SymbolMap};

/// Parse-time reverse tables + teaching forward tables (cloned into [`SymbolMap`] snapshots).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolTables {
    pub sym_to_entity_binding: IndexMap<OpaqueESym, EntityBinding>,
    pub qualified_entity_to_sym: IndexMap<QualifiedEntityKey, OpaqueESym>,
    pub sym_to_method: IndexMap<OpaqueMSym, MethodBinding>,
    pub method_to_sym: IndexMap<MethodKey, OpaqueMSym>,
    pub method_segment_to_sym: HashMap<MethodSegmentKey, OpaqueMSym>,
    pub sym_to_slot: IndexMap<OpaquePSym, SlotBinding>,
    pub entity_field_to_sym: HashMap<EntityFieldKey, OpaquePSym>,
    pub cap_param_to_sym: HashMap<CapParamKey, OpaquePSym>,
    pub sym_to_cap_param_key: HashMap<OpaquePSym, CapParamKey>,
    pub relation_to_sym: HashMap<RelationKey, OpaqueRSym>,
    pub sym_to_relation_binding: IndexMap<OpaqueRSym, RelationBinding>,
}

impl SymbolTables {
    pub fn entity_wire_for_sym(&self, sym: OpaqueESym) -> Option<&str> {
        self.sym_to_entity_binding
            .get(&sym)
            .map(|b| b.entity.as_str())
    }

    pub fn entry_id_for_entity_sym(&self, sym: OpaqueESym) -> Option<&str> {
        self.sym_to_entity_binding
            .get(&sym)
            .map(|b| b.entry_id.as_str())
    }

    pub fn relation_wire_for_sym(&self, sym: OpaqueRSym) -> Option<&str> {
        self.sym_to_relation_binding
            .get(&sym)
            .map(|b| b.relation_wire.as_str())
    }
}

/// Append-only assignment state (not copied wholesale into cross-request cache keys beyond fingerprints).
#[derive(Debug, Default)]
pub struct SymbolLedger {
    pub slot_fingerprint_to_sym: IndexMap<String, OpaquePSym>,
    pub relation_fingerprint_to_sym: IndexMap<String, OpaqueRSym>,
    pub fingerprint_meta: IndexMap<String, IdentMetadata>,
    pub slot_occurrence_meta: IndexMap<String, IdentMetadata>,
    pub value_domain_fp_to_sym: IndexMap<String, OpaqueVSym>,
    pub value_domain_fp_to_repr_meta: IndexMap<String, IdentMetadata>,
    pub symbol_map_cache: RwLock<Option<(u64, Arc<SymbolMap>)>>,
}

impl Clone for SymbolLedger {
    fn clone(&self) -> Self {
        Self {
            slot_fingerprint_to_sym: self.slot_fingerprint_to_sym.clone(),
            relation_fingerprint_to_sym: self.relation_fingerprint_to_sym.clone(),
            fingerprint_meta: self.fingerprint_meta.clone(),
            slot_occurrence_meta: self.slot_occurrence_meta.clone(),
            value_domain_fp_to_sym: self.value_domain_fp_to_sym.clone(),
            value_domain_fp_to_repr_meta: self.value_domain_fp_to_repr_meta.clone(),
            symbol_map_cache: RwLock::new(None),
        }
    }
}

impl SymbolLedger {
    pub fn clear_symbol_map_cache(&self) {
        *self
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned") = None;
    }
}

/// Value-domain gloss + reverse indexes layered on [`SymbolTables`] in read-only [`SymbolMap`].
#[derive(Debug, Clone, Default)]
pub struct SymbolValueLayer {
    pub value_domain_fp_to_sym: IndexMap<String, OpaqueVSym>,
    pub value_sym_to_fp: IndexMap<OpaqueVSym, String>,
    pub p_sym_to_value_sym: HashMap<OpaquePSym, OpaqueVSym>,
    pub value_sym_gloss: IndexMap<OpaqueVSym, String>,
}

impl SymbolValueLayer {
    pub fn build_from_ledger(
        ledger: &SymbolLedger,
        named_value_row_description: impl Fn(&IdentMetadata) -> String,
        render_value_gloss: impl Fn(&IdentMetadata, &str, Option<&CGS>) -> Option<String>,
        catalog_cgs: &IndexMap<String, Arc<CGS>>,
    ) -> Self {
        let mut p_sym_to_value_sym = HashMap::new();
        for (fp, p_sym) in &ledger.slot_fingerprint_to_sym {
            let Some(meta) = ledger.fingerprint_meta.get(fp) else {
                continue;
            };
            let Some(vfp) = meta.value_domain_allocation_fp() else {
                continue;
            };
            if let Some(v_sym) = ledger.value_domain_fp_to_sym.get(&vfp) {
                p_sym_to_value_sym.insert(*p_sym, *v_sym);
            }
        }

        let value_domain_fp_to_sym = ledger.value_domain_fp_to_sym.clone();
        let mut value_sym_to_fp: IndexMap<OpaqueVSym, String> = IndexMap::new();
        for (fp, vs) in &value_domain_fp_to_sym {
            value_sym_to_fp.insert(*vs, fp.clone());
        }

        let mut value_sym_gloss = IndexMap::new();
        for (fp, vsym) in &value_domain_fp_to_sym {
            let Some(meta) = ledger.value_domain_fp_to_repr_meta.get(fp) else {
                continue;
            };
            let nv_desc = named_value_row_description(meta);
            let cgs_opt = catalog_cgs
                .get(meta.catalog_entry_id())
                .map(|arc| arc.as_ref());
            if let Some(g) = render_value_gloss(meta, &nv_desc, cgs_opt) {
                value_sym_gloss.insert(*vsym, g);
            }
        }

        Self {
            value_domain_fp_to_sym,
            value_sym_to_fp,
            p_sym_to_value_sym,
            value_sym_gloss,
        }
    }
}
