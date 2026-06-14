//! Session-scoped context for graph surface rehydrate (hot cache + spill pages).

use plasm_core::CGS;

use crate::execute_session::ExecuteSession;
use crate::server_state::PlasmHostState;

/// Immutable session context shared by all graph-surface walk / rehydrate operations.
pub(crate) struct GraphSurfaceWalkCtx<'a> {
    pub es: &'a ExecuteSession,
    pub st: &'a PlasmHostState,
    pub session_id: &'a str,
    pub cgs: &'a CGS,
}

impl<'a> GraphSurfaceWalkCtx<'a> {
    pub(crate) fn new(
        es: &'a ExecuteSession,
        st: &'a PlasmHostState,
        session_id: &'a str,
        cgs: &'a CGS,
    ) -> Self {
        Self {
            es,
            st,
            session_id,
            cgs,
        }
    }

    pub(crate) fn spill_enabled(&self) -> bool {
        self.st.session_graph_persistence.is_some()
    }
}
