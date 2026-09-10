//! Execute-task tokio task-locals (HTTP base, auth, cancel, federation, …).

use super::{ExecuteSessionMaterial, RowsProgressFn};
use crate::{AuthResolver, CancelSignal};
use std::sync::Arc;

tokio::task_local! {
    /// HTTP origin for compiled relative paths during one [`ExecutionEngine::execute`] (or scoped projection).
    pub(crate) static EXECUTION_HTTP_BASE: Arc<str>;
}

tokio::task_local! {
    /// Optional per-session HTTP auth (registry entry CGS) during one [`ExecutionEngine::execute`] / projection.
    /// When `None` inside the scope, [`ExecutionEngine`] falls back to its constructor [`AuthResolver`].
    pub(crate) static EXECUTION_AUTH_RESOLVER: Option<Arc<AuthResolver>>;
}

tokio::task_local! {
    /// When [`ExecuteOptions::request_fingerprint_sink`] is [`Some`], successful compiled ops append hex fingerprints here.
    pub(crate) static EXECUTION_FINGERPRINT_SINK: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>;
}

tokio::task_local! {
    /// Cooperative cancellation for pagination/hydration loops during one execute task scope.
    pub(crate) static EXECUTION_CANCEL: Option<CancelSignal>;
}

tokio::task_local! {
    pub(crate) static EXECUTION_ROWS_PROGRESS: Option<RowsProgressFn>;
}

tokio::task_local! {
    /// When [`ExecuteOptions::federation`] is set, per-catalog HTTP backends apply per outbound request.
    pub(crate) static EXECUTION_FEDERATION: Option<std::sync::Arc<plasm_core::FederationDispatch>>;
}

tokio::task_local! {
    /// Execute-session identity and the scoped credential persistence adapter.
    pub(crate) static EXECUTION_EXECUTE_SESSION: Option<std::sync::Arc<ExecuteSessionMaterial>>;
}

tokio::task_local! {
    /// Exact parsed request recipes selected before entering execution.
    pub(crate) static EXECUTION_COMPILED_CATALOG: std::sync::Arc<plasm_compile::CompiledCatalog>;
}

tokio::task_local! {
    /// Entity name for the current HTTP op (matches [`plasm_core::FederationDispatch`] keys); selects backend when federated.
    pub(crate) static EXECUTION_DISPATCH_ENTITY: Option<String>;
}
