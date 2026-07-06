//! Single compile-time symbol-map resolution boundary for execute sessions.
//!
//! **Contract:** every `e#` printed in `plasm_context` teaching TSV for this execute row must
//! resolve on the next `plasm` / HTTP execute ingress call against the same
//! `(prompt_hash, session_id)` binding. [`TeachingExposureSession::symbol_map_arc_cross`] memoizes
//! per exposure fingerprint (session-local + optional cross-request LRU) so extend/federate waves
//! cannot return a pre-extend snapshot.

use std::sync::Arc;

use plasm_core::{
    entity_slices_for_render, symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog,
    symbol_map_fingerprint_hex, CgsLayer, FocusSpec, SymbolMap, SymbolMapCrossRequestCache,
    SymbolSession,
};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan_run::session_cgs_layer_stack;

pub struct SessionSymbolMapContext<'a> {
    pub session: &'a ExecuteSession,
    pub cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

/// Hex fingerprint of the session teaching exposure (`hash_exposure_session_rows`).
#[must_use]
pub fn symbol_map_fingerprint_for_session(session: &ExecuteSession) -> Option<String> {
    session
        .teaching_exposure
        .as_ref()
        .map(symbol_map_fingerprint_hex)
}

/// Attach continuity fields agents use to detect symbol-table drift.
pub fn insert_symbol_map_stability_meta(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    session: &ExecuteSession,
) {
    if let Some(fp) = symbol_map_fingerprint_for_session(session) {
        meta.insert("symbol_map_fingerprint".into(), serde_json::json!(fp));
    }
    meta.insert(
        "domain_revision".into(),
        serde_json::json!(session.domain_revision),
    );
}

/// Merge symbol stability fields into MCP/HTTP run `_meta.plasm` (nested or top-level).
pub fn attach_symbol_map_stability_to_run_meta(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    session: &ExecuteSession,
) {
    if let Some(plasm) = meta.get_mut("plasm").and_then(|v| v.as_object_mut()) {
        insert_symbol_map_stability_meta(plasm, session);
        return;
    }
    let mut plasm = serde_json::Map::new();
    insert_symbol_map_stability_meta(&mut plasm, session);
    meta.insert("plasm".into(), serde_json::Value::Object(plasm));
}

#[must_use]
pub fn resolve_session_symbol_map(ctx: &SessionSymbolMapContext<'_>) -> Arc<dyn SymbolSession> {
    let session = ctx.session;
    let stack = session_cgs_layer_stack(session);
    if let Some(e) = session.teaching_exposure.as_ref() {
        let key = if ctx.cross_cache.is_some() {
            if stack.len() <= 1 {
                Some(symbol_map_cache_key_single_catalog(session.cgs.as_ref(), e))
            } else {
                let layers: Vec<_> = stack.iter().map(CgsLayer::cgs).collect();
                Some(symbol_map_cache_key_federated(&layers, e))
            }
        } else {
            None
        };
        e.symbol_map_arc_cross(ctx.cross_cache, key).0
    } else {
        let (full, _) = entity_slices_for_render(session.cgs.as_ref(), FocusSpec::All);
        Arc::new(SymbolMap::build(session.cgs.as_ref(), &full))
    }
}
