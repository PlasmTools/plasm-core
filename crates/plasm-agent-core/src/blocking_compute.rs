//! Semaphore-limited `tokio::task::spawn_blocking` for CPU-bound and sync I/O work.

use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Semaphore;

/// Default max concurrent blocking jobs (caps fan-out CPU thrash on small nodes).
const DEFAULT_MAX_INFLIGHT: usize = 4;

#[derive(Debug, Error)]
pub enum ComputePoolError {
    #[error("compute pool closed")]
    Closed,
    #[error("blocking task join error: {0}")]
    Join(String),
}

/// Bounded wrapper around Tokio's blocking thread pool.
pub struct BlockingComputePool {
    permits: Arc<Semaphore>,
}

impl Default for BlockingComputePool {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockingComputePool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(catalog_compute_max_inflight())),
        }
    }

    #[must_use]
    pub fn with_max_inflight(max_inflight: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_inflight.max(1))),
        }
    }

    pub async fn run<F, T>(&self, label: &'static str, f: F) -> Result<T, ComputePoolError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| ComputePoolError::Closed)?;
        let span = tracing::debug_span!("plasm.blocking_compute", label);
        let out = tokio::task::spawn_blocking(move || {
            let _guard = span.enter();
            f()
        })
        .await
        .map_err(|e| ComputePoolError::Join(e.to_string()))?;
        drop(permit);
        Ok(out)
    }
}

#[must_use]
pub fn catalog_compute_max_inflight() -> usize {
    std::env::var("PLASM_CATALOG_COMPUTE_MAX_INFLIGHT")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n >= 1)
        .unwrap_or_else(default_catalog_compute_max_inflight)
}

#[must_use]
pub fn catalog_materialize_workers() -> usize {
    catalog_compute_max_inflight()
}

fn default_catalog_compute_max_inflight() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, DEFAULT_MAX_INFLIGHT)
}
