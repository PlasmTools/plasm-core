//! Single compile-time symbol-map resolution boundary for execute sessions.

use std::sync::Arc;

use plasm_core::{
    entity_slices_for_render, symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog,
    FocusSpec, SymbolMap, SymbolMapCrossRequestCache,
};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan_run::session_cgs_layers;

pub struct SessionSymbolMapContext<'a> {
    pub session: &'a ExecuteSession,
    pub cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

#[must_use]
pub fn resolve_session_symbol_map(ctx: &SessionSymbolMapContext<'_>) -> Arc<SymbolMap> {
    let session = ctx.session;
    let layers = session_cgs_layers(session);
    if let Some(e) = session.teaching_exposure.as_ref() {
        let key = if ctx.cross_cache.is_some() {
            if layers.len() <= 1 {
                Some(symbol_map_cache_key_single_catalog(session.cgs.as_ref(), e))
            } else {
                Some(symbol_map_cache_key_federated(&layers, e))
            }
        } else {
            None
        };
        Arc::clone(&e.symbol_map_arc_cross(ctx.cross_cache, key).0)
    } else {
        let (full, _) = entity_slices_for_render(session.cgs.as_ref(), FocusSpec::All);
        Arc::new(SymbolMap::build(session.cgs.as_ref(), &full))
    }
}
