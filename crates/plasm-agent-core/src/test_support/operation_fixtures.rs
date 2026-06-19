//! Shared execute-session / host fixtures for operation lifecycle tests.

use std::sync::Arc;

use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::{CatalogBootstrap, PlasmHostState};
use crate::trace_sink_emit::PlasmTraceContext;

pub fn minimal_host() -> Arc<PlasmHostState> {
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    Arc::new(build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(InMemoryCgsRegistry::from_pairs(Vec::new())),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    }))
}

pub fn plain_trace() -> PlasmTraceContext {
    PlasmTraceContext {
        trace_id: uuid::Uuid::nil(),
        call_index: None,
        mcp_session_id: None,
        logical_session_id: None,
        logical_session_ref: None,
    }
}
