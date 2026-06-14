mod in_memory_session_store;
use crate::mcp_server::ServerRuntime;
use async_trait::async_trait;
pub use in_memory_session_store::*;
use rust_mcp_transport::SessionId;
use std::sync::Arc;

/// Trait defining the interface for session storage operations
///
/// This trait provides asynchronous methods for managing session data,
/// Implementors must be Send and Sync to support concurrent access.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Retrieves a session by its identifier
    ///
    /// # Arguments
    /// * `key` - The session identifier to look up
    ///
    /// # Returns
    /// * `Option<Arc<ServerRuntime>>` - The session stream if found, None otherwise
    async fn get(&self, key: &SessionId) -> Option<Arc<ServerRuntime>>;
    /// Stores a new session with the given identifier
    ///
    /// # Arguments
    /// * `key` - The session identifier
    /// * `value` - The duplex stream to store
    async fn set(&self, key: SessionId, value: Arc<ServerRuntime>);
    /// Drops the in-process runtime cache for this session (SSE stream teardown, pod shutdown).
    ///
    /// Redis-backed stores keep remote metadata until [`Self::delete_persistent`].
    async fn delete(&self, key: &SessionId);

    /// Client-initiated session teardown (HTTP DELETE): local cache + durable metadata.
    async fn delete_persistent(&self, key: &SessionId) {
        self.delete(key).await;
    }

    async fn has(&self, session: &SessionId) -> bool;

    async fn keys(&self) -> Vec<SessionId>;

    async fn values(&self) -> Vec<Arc<ServerRuntime>>;

    /// Clears all sessions from the store
    async fn clear(&self);

    /// Persist session metadata for cross-pod hydration (default: no-op).
    async fn persist_session_metadata(&self, _key: &SessionId, _init_payload: Option<&str>) {}

    /// Refresh remote TTL on activity (default: no-op).
    async fn touch_session(&self, _key: &SessionId) {}

    /// Whether a session record exists remotely (default: local `has` only).
    async fn exists_remote(&self, key: &SessionId) -> bool {
        self.has(key).await
    }
}
