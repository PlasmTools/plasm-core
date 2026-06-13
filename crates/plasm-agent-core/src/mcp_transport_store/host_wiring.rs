//! Wire shared Redis stores onto [`PlasmHostState`] (execute bindings + session descriptors).

use std::sync::Arc;

use crate::server_state::PlasmHostState;

use super::config::McpTransportStoreConfig;
use super::redis_backend::RedisBackend;

/// Connect Redis when `PLASM_MCP_TRANSPORT_REDIS_URL` is set (sole factory for all transport stores).
pub async fn connect_redis_backend() -> Result<Option<Arc<RedisBackend>>, String> {
    let Some(cfg) = McpTransportStoreConfig::from_env() else {
        return Ok(None);
    };
    let backend = Arc::new(
        RedisBackend::connect(&cfg.redis_url, cfg.ttl)
            .await
            .map_err(|e| format!("Redis connect failed: {e}"))?,
    );
    backend
        .ping()
        .await
        .map_err(|e| format!("Redis ping failed: {e}"))?;
    Ok(Some(backend))
}

/// Attach one shared backend to execute bindings, session descriptors, and host state.
pub async fn wire_host_redis(
    plasm: &mut PlasmHostState,
    backend: Arc<RedisBackend>,
) -> Result<(), String> {
    backend
        .ping()
        .await
        .map_err(|e| format!("Redis ping failed: {e}"))?;
    plasm
        .logical_execute_bindings
        .attach_redis(Arc::clone(&backend))
        .await;
    plasm
        .logical_sessions
        .attach_redis(Arc::clone(&backend))
        .await;
    plasm
        .execute_session_registry
        .attach_redis(Arc::clone(&backend))
        .await;
    plasm.oss.redis_backend = Some(backend);
    tracing::info!("execute + MCP transport stores: shared Redis (multi-replica safe)");
    Ok(())
}

/// Connect Redis (when configured) and wire all host stores before serve/MCP bootstrap.
pub async fn prepare_host_for_serve(mut state: PlasmHostState) -> Result<PlasmHostState, String> {
    if let Some(backend) = connect_redis_backend().await? {
        wire_host_redis(&mut state, backend).await?;
    }
    Ok(state)
}
