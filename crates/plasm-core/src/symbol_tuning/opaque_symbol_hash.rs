//! Opaque `e#`/`m#`/`p#`/`r#`/`v#` assignment fingerprint for [`SymbolMapCrossRequestCache`] keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::TeachingExposureSession;

/// Hex fingerprint of wave-order-dependent opaque assignment (for MCP `_meta.plasm` continuity).
#[must_use]
pub fn symbol_map_fingerprint_hex(exposure: &TeachingExposureSession) -> String {
    format!("{:016x}", hash_exposure_session_rows(exposure))
}

/// Hash wave-order-dependent opaque assignment (must not collide across different wave histories).
pub(crate) fn hash_exposure_session_rows(exposure: &TeachingExposureSession) -> u64 {
    let mut h = DefaultHasher::new();
    for (e, row) in exposure
        .entities
        .iter()
        .zip(&exposure.entity_catalog_entry_ids)
    {
        e.hash(&mut h);
        row.hash(&mut h);
    }
    exposure.surface.fingerprint().hash(&mut h);
    for (key, sym) in &exposure.tables.qualified_entity_to_sym {
        key.entry_id.as_str().hash(&mut h);
        key.entity.as_str().hash(&mut h);
        sym.as_wire().hash(&mut h);
    }
    for (key, sym) in &exposure.tables.method_to_sym {
        key.entry_id.as_str().hash(&mut h);
        key.domain.as_str().hash(&mut h);
        key.capability.as_str().hash(&mut h);
        sym.as_wire().hash(&mut h);
    }
    for (fp, sym) in &exposure.ledger.slot_fingerprint_to_sym {
        fp.hash(&mut h);
        sym.as_wire().hash(&mut h);
    }
    for (fp, sym) in &exposure.ledger.relation_fingerprint_to_sym {
        fp.hash(&mut h);
        sym.as_wire().hash(&mut h);
    }
    for (fp, sym) in &exposure.ledger.value_domain_fp_to_sym {
        fp.hash(&mut h);
        sym.as_wire().hash(&mut h);
    }
    h.finish()
}
