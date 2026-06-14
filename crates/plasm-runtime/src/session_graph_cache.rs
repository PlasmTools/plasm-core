//! Session-scoped exclusive access to [`SessionMaterialization`] for async execute scopes.
//!
//! Callers hold one [`MutexGraphCacheSession`] per HTTP/MCP execute session (plasm) instead of a
//! process-global mutex. See cache module invariants **I5** (single writer).
//!
//! Live execute should fork materialization under lock, mutate the branch across awaited
//! HTTP, then commit with an epoch check — the mutex must not be held during I/O.

use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard};

use crate::materialization::SessionMaterialization;
use crate::GraphCache;

/// Wraps `Arc<Mutex<SessionMaterialization>>` so HTTP/MCP call sites do not thread `Arc<Mutex<…>>` through helpers.
///
/// Do **not** hold two concurrent locks on the same session (deadlock).
#[derive(Clone)]
pub struct MutexGraphCacheSession {
    inner: Arc<Mutex<SessionMaterialization>>,
}

impl MutexGraphCacheSession {
    pub fn new(cache: GraphCache) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionMaterialization {
                graph: cache,
                ..SessionMaterialization::default()
            })),
        }
    }

    pub fn new_materialization(mat: SessionMaterialization) -> Self {
        Self {
            inner: Arc::new(Mutex::new(mat)),
        }
    }

    /// Exclusive access; keep the guard alive across awaited `ExecutionEngine::execute` / projection work.
    pub async fn lock(&self) -> MutexGuard<'_, SessionMaterialization> {
        self.inner.lock().await
    }

    /// Deep copy for parallel batch fork-merge (each line runs against this snapshot).
    pub async fn snapshot(&self) -> SessionMaterialization {
        self.inner.lock().await.clone()
    }
}
