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
    CgsLayer, FocusSpec, SymbolMap, SymbolMapCrossRequestCache, SymbolSession,
};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan_run::session_cgs_layer_stack;

pub struct SessionSymbolMapContext<'a> {
    pub session: &'a ExecuteSession,
    pub cross_cache: Option<&'a SymbolMapCrossRequestCache>,
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
