//! Opaque `e#`/`m#`/`p#`/`r#`/`v#` assignment fingerprint for [`SymbolMapCrossRequestCache`] keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::TeachingExposureSession;

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
    for ((entry, entity), sym) in &exposure.qualified_entity_to_sym {
        entry.hash(&mut h);
        entity.hash(&mut h);
        sym.hash(&mut h);
    }
    for ((entry, entity, method), sym) in &exposure.method_to_sym {
        entry.hash(&mut h);
        entity.hash(&mut h);
        method.hash(&mut h);
        sym.hash(&mut h);
    }
    for (fp, sym) in &exposure.slot_fingerprint_to_sym {
        fp.hash(&mut h);
        sym.hash(&mut h);
    }
    for (fp, sym) in &exposure.relation_fingerprint_to_sym {
        fp.hash(&mut h);
        sym.hash(&mut h);
    }
    for (fp, sym) in &exposure.value_domain_fp_to_sym {
        fp.hash(&mut h);
        sym.hash(&mut h);
    }
    h.finish()
}
