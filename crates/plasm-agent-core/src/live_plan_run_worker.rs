//! Bounded large-stack worker pool for live `run_plasm_comp` (avoids tokio 2 MiB worker overflow).

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use tokio::sync::{oneshot, Semaphore};

use crate::execute_session::max_running_ops_per_session;

/// Debug builds: deep synchronous `run_plasm_comp` stacks (matrix / federated compile).
pub const DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_DEBUG: usize = 16 * 1024 * 1024;
/// Release builds: profiled default for cheap pokeapi-style plans on dedicated workers.
pub const DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_RELEASE: usize = 4 * 1024 * 1024;

/// Back-compat alias (debug default).
pub const DEFAULT_LIVE_PLAN_RUN_STACK_BYTES: usize = DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_DEBUG;

#[must_use]
pub fn default_live_plan_run_stack_bytes() -> usize {
    if cfg!(debug_assertions) {
        DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_DEBUG
    } else {
        DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_RELEASE
    }
}

/// `PLASM_LIVE_RUN_STACK_BYTES` — per-worker stack for live plan execution.
#[must_use]
pub fn live_plan_run_stack_bytes() -> usize {
    std::env::var("PLASM_LIVE_RUN_STACK_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n >= 512 * 1024)
        .unwrap_or_else(default_live_plan_run_stack_bytes)
}

/// Bounded worker pool sized to [`max_running_ops_per_session`].
pub struct LivePlanRunPool {
    stack_size: usize,
    permits: Arc<Semaphore>,
}

impl Default for LivePlanRunPool {
    fn default() -> Self {
        Self::new()
    }
}

impl LivePlanRunPool {
    #[must_use]
    pub fn with_stack_bytes(stack_size: usize) -> Self {
        Self {
            stack_size,
            permits: Arc::new(Semaphore::new(max_running_ops_per_session())),
        }
    }

    #[must_use]
    pub fn new() -> Self {
        Self::with_stack_bytes(live_plan_run_stack_bytes())
    }

    #[must_use]
    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    /// Run `f` on a dedicated large-stack thread (`block_on` on the current runtime handle).
    pub async fn run<F, Fut, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send,
        T: Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "live plan run pool closed".to_string())?;
        let stack_size = self.stack_size;
        let rt = tokio::runtime::Handle::current();
        let (done_tx, done_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("plasm-live-run".into())
            .stack_size(stack_size)
            .spawn(move || {
                let out = std::panic::catch_unwind(AssertUnwindSafe(|| rt.block_on(f())));
                let msg = match out {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("live plan run panicked".to_string()),
                };
                let _ = done_tx.send(msg);
            })
            .map_err(|e| format!("spawn live plan run worker: {e}"))?;
        let result = done_rx
            .await
            .map_err(|_| "live plan run worker dropped result".to_string())?;
        drop(permit);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stack_matches_profile() {
        assert_eq!(
            live_plan_run_stack_bytes(),
            default_live_plan_run_stack_bytes()
        );
    }

    #[test]
    fn release_default_is_smaller_than_debug_cap() {
        if cfg!(debug_assertions) {
            assert_eq!(
                default_live_plan_run_stack_bytes(),
                DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_DEBUG
            );
        } else {
            assert_eq!(
                default_live_plan_run_stack_bytes(),
                DEFAULT_LIVE_PLAN_RUN_STACK_BYTES_RELEASE
            );
        }
    }
}
