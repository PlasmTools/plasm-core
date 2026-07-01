//! Versioned postcard snapshot of append-only teaching symbol state for cross-pod rehydrate.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::schema::CGS;

use super::capability_surface_params::loaded_catalog_entry_ids;
use super::persisted_ident_metadata::PersistedIdentMetadata;
use super::session_bindings::{EntityBinding, MethodBinding, RelationBinding, SlotBinding};
use super::tables::{SymbolLedger, SymbolTables};
use super::{
    CapParamKey, EntityFieldKey, ExposureSurface, IdentMetadata, MethodKey, MethodSegmentKey,
    OpaqueESym, OpaqueMSym, OpaquePSym, OpaqueRSym, QualifiedEntityKey, RelationKey,
    TeachingExposureSession,
};

pub const PERSISTED_SYMBOL_LEDGER_VERSION: u8 = 1;

const MAGIC: &[u8; 4] = b"PLSL";

/// Postcard-safe mirror of [`ExposureSurface`] (`BTreeSet` → sorted `Vec`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedExposureSurface {
    pub entities: Vec<super::ExposureEntityKey>,
    pub capabilities: Vec<super::ExposureCapabilityKey>,
    pub slots: Vec<super::ExposureSlotKey>,
}

impl From<&ExposureSurface> for PersistedExposureSurface {
    fn from(surface: &ExposureSurface) -> Self {
        Self {
            entities: surface.entities.iter().cloned().collect(),
            capabilities: surface.capabilities.iter().cloned().collect(),
            slots: surface.slots.iter().cloned().collect(),
        }
    }
}

impl From<PersistedExposureSurface> for ExposureSurface {
    fn from(surface: PersistedExposureSurface) -> Self {
        Self {
            entities: surface.entities.into_iter().collect(),
            capabilities: surface.capabilities.into_iter().collect(),
            slots: surface.slots.into_iter().collect(),
        }
    }
}

/// Postcard-safe mirror of [`SymbolTables`] (`HashMap` fields become [`IndexMap`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedSymbolTables {
    pub sym_to_entity_binding: IndexMap<OpaqueESym, EntityBinding>,
    pub qualified_entity_to_sym: IndexMap<QualifiedEntityKey, OpaqueESym>,
    pub sym_to_method: IndexMap<OpaqueMSym, MethodBinding>,
    pub method_to_sym: IndexMap<MethodKey, OpaqueMSym>,
    pub method_segment_to_sym: IndexMap<MethodSegmentKey, OpaqueMSym>,
    pub sym_to_slot: IndexMap<OpaquePSym, SlotBinding>,
    pub entity_field_to_sym: IndexMap<EntityFieldKey, OpaquePSym>,
    pub cap_param_to_sym: IndexMap<CapParamKey, OpaquePSym>,
    pub sym_to_cap_param_key: IndexMap<OpaquePSym, CapParamKey>,
    pub relation_to_sym: IndexMap<RelationKey, OpaqueRSym>,
    pub sym_to_relation_binding: IndexMap<OpaqueRSym, RelationBinding>,
}

impl From<&SymbolTables> for PersistedSymbolTables {
    fn from(tables: &SymbolTables) -> Self {
        Self {
            sym_to_entity_binding: tables.sym_to_entity_binding.clone(),
            qualified_entity_to_sym: tables.qualified_entity_to_sym.clone(),
            sym_to_method: tables.sym_to_method.clone(),
            method_to_sym: tables.method_to_sym.clone(),
            method_segment_to_sym: tables.method_segment_to_sym.clone().into_iter().collect(),
            sym_to_slot: tables.sym_to_slot.clone(),
            entity_field_to_sym: tables.entity_field_to_sym.clone().into_iter().collect(),
            cap_param_to_sym: tables.cap_param_to_sym.clone().into_iter().collect(),
            sym_to_cap_param_key: tables.sym_to_cap_param_key.clone().into_iter().collect(),
            relation_to_sym: tables.relation_to_sym.clone().into_iter().collect(),
            sym_to_relation_binding: tables.sym_to_relation_binding.clone(),
        }
    }
}

impl From<PersistedSymbolTables> for SymbolTables {
    fn from(tables: PersistedSymbolTables) -> Self {
        Self {
            sym_to_entity_binding: tables.sym_to_entity_binding,
            qualified_entity_to_sym: tables.qualified_entity_to_sym,
            sym_to_method: tables.sym_to_method,
            method_to_sym: tables.method_to_sym,
            method_segment_to_sym: tables.method_segment_to_sym.into_iter().collect(),
            sym_to_slot: tables.sym_to_slot,
            entity_field_to_sym: tables.entity_field_to_sym.into_iter().collect(),
            cap_param_to_sym: tables.cap_param_to_sym.into_iter().collect(),
            sym_to_cap_param_key: tables.sym_to_cap_param_key.into_iter().collect(),
            relation_to_sym: tables.relation_to_sym.into_iter().collect(),
            sym_to_relation_binding: tables.sym_to_relation_binding,
        }
    }
}

/// Serializable [`SymbolLedger`] assignment maps (excludes ephemeral `symbol_map_cache`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedSymbolLedgerState {
    pub slot_fingerprint_to_sym: IndexMap<String, OpaquePSym>,
    pub relation_fingerprint_to_sym: IndexMap<String, OpaqueRSym>,
    pub fingerprint_meta: IndexMap<String, PersistedIdentMetadata>,
    pub slot_occurrence_meta: IndexMap<String, PersistedIdentMetadata>,
    pub value_domain_fp_to_sym: IndexMap<String, super::OpaqueVSym>,
    pub value_domain_fp_to_repr_meta: IndexMap<String, PersistedIdentMetadata>,
}

fn meta_map_to_wire(
    map: &IndexMap<String, IdentMetadata>,
) -> Result<IndexMap<String, PersistedIdentMetadata>, PersistedSymbolLedgerEncodeError> {
    map.iter()
        .map(|(k, v)| Ok((k.clone(), PersistedIdentMetadata::from(v))))
        .collect()
}

fn meta_map_from_wire(
    map: IndexMap<String, PersistedIdentMetadata>,
) -> Result<IndexMap<String, IdentMetadata>, PersistedSymbolLedgerDecodeError> {
    map.into_iter()
        .map(|(k, v)| {
            let fingerprint = k.clone();
            v.into_ident_metadata()
                .map(|meta| (k, meta))
                .map_err(|reason| PersistedSymbolLedgerDecodeError::IdentMetadata {
                    fingerprint,
                    reason,
                })
        })
        .collect()
}

impl PersistedSymbolLedgerState {
    pub fn from_ledger(ledger: &SymbolLedger) -> Result<Self, PersistedSymbolLedgerEncodeError> {
        Ok(Self {
            slot_fingerprint_to_sym: ledger.slot_fingerprint_to_sym.clone(),
            relation_fingerprint_to_sym: ledger.relation_fingerprint_to_sym.clone(),
            fingerprint_meta: meta_map_to_wire(&ledger.fingerprint_meta)?,
            slot_occurrence_meta: meta_map_to_wire(&ledger.slot_occurrence_meta)?,
            value_domain_fp_to_sym: ledger.value_domain_fp_to_sym.clone(),
            value_domain_fp_to_repr_meta: meta_map_to_wire(&ledger.value_domain_fp_to_repr_meta)?,
        })
    }

    pub fn into_symbol_ledger(self) -> Result<SymbolLedger, PersistedSymbolLedgerDecodeError> {
        Ok(SymbolLedger {
            slot_fingerprint_to_sym: self.slot_fingerprint_to_sym,
            relation_fingerprint_to_sym: self.relation_fingerprint_to_sym,
            fingerprint_meta: meta_map_from_wire(self.fingerprint_meta)?,
            slot_occurrence_meta: meta_map_from_wire(self.slot_occurrence_meta)?,
            value_domain_fp_to_sym: self.value_domain_fp_to_sym,
            value_domain_fp_to_repr_meta: meta_map_from_wire(self.value_domain_fp_to_repr_meta)?,
            symbol_map_cache: Default::default(),
        })
    }
}

/// Compact durable symbol numbering authority (no CGS bodies, prompts, or TSV).
/// Wire version is carried only in the `PLSL` envelope byte (not duplicated in the postcard body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedSymbolLedger {
    pub catalog_cgs_hashes: IndexMap<String, String>,
    pub entities: Vec<String>,
    pub entity_catalog_entry_ids: Vec<String>,
    pub surface: PersistedExposureSurface,
    pub tables: PersistedSymbolTables,
    pub ledger: PersistedSymbolLedgerState,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistedSymbolLedgerEncodeError {
    #[error("postcard encode failed: {0}")]
    Postcard(String),
    #[error("ident metadata encode failed: {0}")]
    IdentMetadata(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PersistedSymbolLedgerDecodeError {
    #[error("symbol ledger blob too short")]
    TooShort,
    #[error("invalid symbol ledger magic")]
    BadMagic,
    #[error("unsupported symbol ledger version {0}")]
    UnsupportedVersion(u8),
    #[error("postcard decode failed: {0}")]
    Postcard(String),
    #[error("ident metadata decode failed for fingerprint `{fingerprint}`: {reason}")]
    IdentMetadata { fingerprint: String, reason: String },
}

impl PersistedSymbolLedger {
    pub fn from_session(
        exp: &TeachingExposureSession,
        catalog_cgs_hashes: IndexMap<String, String>,
    ) -> Result<Self, PersistedSymbolLedgerEncodeError> {
        Ok(Self {
            catalog_cgs_hashes,
            entities: exp.entities.clone(),
            entity_catalog_entry_ids: exp.entity_catalog_entry_ids.clone(),
            surface: PersistedExposureSurface::from(&exp.surface),
            tables: PersistedSymbolTables::from(exp.tables()),
            ledger: PersistedSymbolLedgerState::from_ledger(exp.ledger())?,
        })
    }

    /// Rebuild a live session from persisted tables + rematerialized CGS graphs (no exposure replay).
    pub fn hydrate(
        &self,
        catalog_cgs: &IndexMap<String, Arc<CGS>>,
    ) -> Result<TeachingExposureSession, PersistedSymbolLedgerDecodeError> {
        if self.entities.len() != self.entity_catalog_entry_ids.len() {
            return Err(PersistedSymbolLedgerDecodeError::Postcard(
                "persisted entity/catalog pairing length mismatch".into(),
            ));
        }
        let mut session = TeachingExposureSession::from_persisted(
            self.surface.clone().into(),
            self.entities.clone(),
            self.entity_catalog_entry_ids.clone(),
            catalog_cgs.clone(),
            self.tables.clone().into(),
            self.ledger.clone().into_symbol_ledger()?,
        );
        session.rebuild_ident_meta_from_ledger();
        Ok(session)
    }

    pub fn encode(&self) -> Result<Vec<u8>, PersistedSymbolLedgerEncodeError> {
        let body = postcard::to_allocvec(self)
            .map_err(|e| PersistedSymbolLedgerEncodeError::Postcard(e.to_string()))?;
        let mut out = Vec::with_capacity(MAGIC.len() + 1 + body.len());
        out.extend_from_slice(MAGIC);
        out.push(PERSISTED_SYMBOL_LEDGER_VERSION);
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistedSymbolLedgerDecodeError> {
        if bytes.len() < MAGIC.len() + 1 {
            return Err(PersistedSymbolLedgerDecodeError::TooShort);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(PersistedSymbolLedgerDecodeError::BadMagic);
        }
        let wire_version = bytes[MAGIC.len()];
        if wire_version != PERSISTED_SYMBOL_LEDGER_VERSION {
            return Err(PersistedSymbolLedgerDecodeError::UnsupportedVersion(
                wire_version,
            ));
        }
        postcard::from_bytes(&bytes[MAGIC.len() + 1..])
            .map_err(|e| PersistedSymbolLedgerDecodeError::Postcard(e.to_string()))
    }
}

/// Collect pinned **effective** catalog digests for every registry row loaded in this session.
pub fn catalog_cgs_hashes_from_session(exp: &TeachingExposureSession) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for entry_id in loaded_catalog_entry_ids(exp) {
        if let Some(cgs) = exp.catalog_cgs_for_entry(entry_id.as_str()) {
            out.insert(entry_id, cgs.effective_catalog_cgs_hash_hex());
        }
    }
    out
}

/// Verify rematerialized CGS digests match a pinned hash map (same contract as execute rehydrate).
pub fn catalog_pins_match(
    pinned: &IndexMap<String, String>,
    live: &IndexMap<String, Arc<CGS>>,
) -> bool {
    pinned.len() == live.len()
        && pinned.iter().all(|(entry_id, expected)| {
            live.get(entry_id)
                .is_some_and(|cgs| cgs.effective_catalog_cgs_hash_hex() == *expected)
        })
}

impl TeachingExposureSession {
    pub(crate) fn tables(&self) -> &SymbolTables {
        &self.tables
    }

    pub(crate) fn ledger(&self) -> &SymbolLedger {
        &self.ledger
    }

    pub(crate) fn from_persisted(
        surface: ExposureSurface,
        entities: Vec<String>,
        entity_catalog_entry_ids: Vec<String>,
        catalog_cgs: IndexMap<String, Arc<CGS>>,
        tables: SymbolTables,
        ledger: SymbolLedger,
    ) -> Self {
        Self {
            surface,
            entities,
            entity_catalog_entry_ids,
            catalog_cgs,
            tables,
            ledger,
            ident_meta_by_entity: HashMap::new(),
        }
    }

    pub(crate) fn rebuild_ident_meta_from_ledger(&mut self) {
        self.ident_meta_by_entity.clear();
        for meta in self.ledger.slot_occurrence_meta.values() {
            let key = (meta.catalog_entry_id().to_string(), meta.entity().clone());
            self.ident_meta_by_entity
                .entry(key)
                .or_default()
                .entry(meta.wire_name().to_string())
                .or_insert_with(|| meta.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    fn round_trip(exp: &TeachingExposureSession) -> TeachingExposureSession {
        let hashes = catalog_cgs_hashes_from_session(exp);
        let snap = PersistedSymbolLedger::from_session(exp, hashes).expect("from_session");
        let encoded = snap.encode().expect("encode");
        let decoded = PersistedSymbolLedger::decode(&encoded).expect("decode");
        let entry_id = exp
            .entity_catalog_entry_ids
            .first()
            .cloned()
            .unwrap_or_default();
        let cgs = exp
            .catalog_cgs_for_entry(entry_id.as_str())
            .expect("catalog cgs");
        let mut catalog_cgs = IndexMap::new();
        catalog_cgs.insert(entry_id, Arc::new(cgs.clone()));
        decoded.hydrate(&catalog_cgs).expect("hydrate")
    }

    #[test]
    fn persisted_symbol_ledger_round_trip_preserves_entity_symbols() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp =
            TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
        let restored = round_trip(&exp);
        assert_eq!(
            exp.qualified_entity_symbol("langmatrix", "HomographRowA"),
            restored.qualified_entity_symbol("langmatrix", "HomographRowA"),
        );
        assert_eq!(
            exp.qualified_entity_symbol("langmatrix", "HomographRowB"),
            restored.qualified_entity_symbol("langmatrix", "HomographRowB"),
        );
    }

    #[test]
    fn persisted_symbol_ledger_version_mismatch_rejected() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA"]);
        let mut bytes = PersistedSymbolLedger::from_session(&exp, IndexMap::new())
            .expect("from_session")
            .encode()
            .expect("encode");
        bytes[MAGIC.len()] = 99;
        assert!(matches!(
            PersistedSymbolLedger::decode(&bytes),
            Err(PersistedSymbolLedgerDecodeError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn persisted_symbol_ledger_github_round_trip_when_catalog_present() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("github");
        let exp = TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue", "Label"]);
        let restored = round_trip(&exp);
        assert_eq!(
            exp.qualified_entity_symbol("github", "Label"),
            restored.qualified_entity_symbol("github", "Label")
        );
    }

    #[test]
    fn catalog_pins_match_uses_effective_hash() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA"]);
        let pinned = catalog_cgs_hashes_from_session(&exp);
        let mut live = IndexMap::new();
        live.insert("langmatrix".to_string(), Arc::new(cgs.clone()));
        assert!(catalog_pins_match(&pinned, &live));
        let mut wrong = pinned.clone();
        wrong.insert("langmatrix".to_string(), "deadbeef".into());
        assert!(!catalog_pins_match(&wrong, &live));
    }

    #[test]
    fn persisted_symbol_ledger_postcard_size_benchmark() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp =
            TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
        let hashes = catalog_cgs_hashes_from_session(&exp);
        let snap = PersistedSymbolLedger::from_session(&exp, hashes).expect("from_session");
        let postcard_len = snap.encode().expect("encode").len();
        assert!(
            postcard_len <= 32 * 1024,
            "matrix fixture blob should stay under 32 KiB (got {postcard_len} B)"
        );
    }
}
