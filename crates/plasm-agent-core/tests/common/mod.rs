//! Shared fixtures for integration tests under `tests/`.

use std::sync::Arc;

use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::operation::OpAcceptContext;
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::{CatalogBootstrap, PlasmHostState};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::OperationHandle;
use plasm_runtime::{CancelSignal, ExecutionConfig, ExecutionEngine, ExecutionMode};

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

pub fn empty_session() -> Arc<ExecuteSession> {
    Arc::new(ExecuteSession::new(
        "ph".into(),
        "p".into(),
        Arc::new(plasm_core::CGS::new()),
        indexmap::IndexMap::new(),
        "default".into(),
        String::new(),
        String::new(),
        None,
        vec!["Pet".into()],
        None,
        None,
        None,
        "hash".into(),
        None,
        None,
    ))
}

pub fn begin_plain_operation(es: &ExecuteSession) -> OperationHandle {
    let handle = es.mint_operation_handle_plain();
    es.try_begin_async_operation(
        handle.clone(),
        CancelSignal::new(),
        OpAcceptContext::default(),
    )
    .expect("begin");
    handle
}
