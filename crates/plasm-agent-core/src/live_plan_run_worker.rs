//! Bounded worker pool for live `run_plasm_comp`, with a normal stack in every build profile.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use thiserror::Error;

use tokio::sync::{oneshot, Semaphore};

use crate::execute_session::max_running_ops_per_session;

/// Same normal worker budget in debug and release builds.
pub const DEFAULT_LIVE_PLAN_RUN_STACK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum LivePlanRunError<E: std::error::Error + 'static> {
    #[error("live plan run pool is closed")]
    PoolClosed,
    #[error("failed to spawn live plan run worker")]
    Spawn(#[source] std::io::Error),
    #[error("live plan run worker dropped its result")]
    ResultDropped,
    #[error("live plan run panicked: {payload}")]
    Panicked {
        #[source]
        payload: LivePlanRunPanicPayload,
    },
    #[error(transparent)]
    Task(#[from] E),
}

impl From<LivePlanRunError<std::convert::Infallible>>
    for LivePlanRunError<plasm_runtime::ExecutionFailure>
{
    fn from(error: LivePlanRunError<std::convert::Infallible>) -> Self {
        match error {
            LivePlanRunError::PoolClosed => Self::PoolClosed,
            LivePlanRunError::Spawn(source) => Self::Spawn(source),
            LivePlanRunError::ResultDropped => Self::ResultDropped,
            LivePlanRunError::Panicked { payload } => Self::Panicked { payload },
            LivePlanRunError::Task(never) => match never {},
        }
    }
}

impl LivePlanRunError<plasm_runtime::ExecutionFailure> {
    pub fn into_execution_failure(self) -> plasm_runtime::ExecutionFailure {
        match self {
            Self::Task(error) => error,
            Self::PoolClosed => plasm_runtime::ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "live_plan_worker_pool_closed",
                "live plan run worker pool is closed",
            ),
            Self::Spawn(source) => plasm_runtime::ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "live_plan_worker_spawn_failed",
                format!("could not start live plan worker: {source}"),
            ),
            Self::ResultDropped => plasm_runtime::ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "live_plan_worker_result_dropped",
                "live plan worker dropped its result",
            ),
            Self::Panicked { payload } => plasm_runtime::ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "live_plan_worker_panicked",
                payload.to_string(),
            ),
        }
    }
}

/// `PLASM_LIVE_RUN_STACK_BYTES` — per-worker stack for live plan execution.
#[must_use]
pub fn live_plan_run_stack_bytes() -> usize {
    std::env::var("PLASM_LIVE_RUN_STACK_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n >= 512 * 1024)
        .unwrap_or(DEFAULT_LIVE_PLAN_RUN_STACK_BYTES)
}

/// Owned unwind evidence. The mutex makes a `Send`-only panic payload safe to share
/// through error boundaries without requiring the original payload to be `Sync`.
pub struct LivePlanRunPanicPayload(Mutex<Box<dyn Any + Send>>);

impl LivePlanRunPanicPayload {
    fn new(payload: Box<dyn Any + Send>) -> Self {
        Self(Mutex::new(payload))
    }

    pub fn into_inner(self) -> Box<dyn Any + Send> {
        self.0
            .into_inner()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl std::fmt::Display for LivePlanRunPanicPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let payload = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(message) = payload.downcast_ref::<&str>() {
            return f.write_str(message);
        }
        if let Some(message) = payload.downcast_ref::<String>() {
            return f.write_str(message);
        }
        write!(
            f,
            "non-string panic payload (type {:?})",
            (**payload).type_id()
        )
    }
}

impl std::fmt::Debug for LivePlanRunPanicPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for LivePlanRunPanicPayload {}

/// Bounded worker pool sized to [`max_running_ops_per_session`].
pub struct LivePlanRunPool {
    stack_size: usize,
    permits: Arc<Semaphore>,
}

/// Owns the result of an admitted worker independently of any single waiter.
/// Cancelling a wait does not cancel the worker or release its permit.
pub struct LivePlanRunHandle<T, E: std::error::Error + 'static> {
    result: Option<oneshot::Receiver<Result<T, LivePlanRunError<E>>>>,
}

impl<T, E: std::error::Error + 'static> LivePlanRunHandle<T, E> {
    /// Wait for the worker result. This wait can be cancelled and retried.
    pub async fn wait(&mut self) -> Result<T, LivePlanRunError<E>> {
        let receiver = self
            .result
            .as_mut()
            .ok_or(LivePlanRunError::ResultDropped)?;
        let result = receiver.await;
        self.result = None;
        result.map_err(|_| LivePlanRunError::ResultDropped)?
    }
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

    /// Run `f` on a dedicated worker thread (`block_on` on the current runtime handle).
    pub async fn run<F, Fut, T, E>(&self, f: F) -> Result<T, LivePlanRunError<E>>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.run_impl(f).await
    }

    /// Like [`Self::run`], but the future may be `!Send` (created and polled only on the worker).
    ///
    /// Use for MCP tool handlers whose error type is `Box<dyn Error>` (`CallToolError`).
    pub async fn run_local<F, Fut, T>(
        &self,
        f: F,
    ) -> Result<T, LivePlanRunError<std::convert::Infallible>>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T>,
        T: Send + 'static,
    {
        self.run_impl(|| async move { Ok::<T, std::convert::Infallible>(f().await) })
            .await
    }

    /// Admit a local future while retaining its result for later polling.
    /// Dropping the handle does not cancel the worker.
    pub async fn start_local<F, Fut, T>(
        &self,
        f: F,
    ) -> Result<
        LivePlanRunHandle<T, std::convert::Infallible>,
        LivePlanRunError<std::convert::Infallible>,
    >
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T>,
        T: Send + 'static,
    {
        self.start_impl(|| async move { Ok::<T, std::convert::Infallible>(f().await) })
            .await
    }

    async fn run_impl<F, Fut, T, E>(&self, f: F) -> Result<T, LivePlanRunError<E>>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>>,
        T: Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.start_impl(f).await?.wait().await
    }

    async fn start_impl<F, Fut, T, E>(
        &self,
        f: F,
    ) -> Result<LivePlanRunHandle<T, E>, LivePlanRunError<E>>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>>,
        T: Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LivePlanRunError::PoolClosed)?;
        let stack_size = self.stack_size;
        let rt = tokio::runtime::Handle::current();
        let (done_tx, done_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("plasm-live-run".into())
            .stack_size(stack_size)
            .spawn(move || {
                // A cancelled caller drops `done_rx`, but cannot stop this worker.
                // Keep its admission permit until the worker itself exits.
                let _permit = permit;
                let out = std::panic::catch_unwind(AssertUnwindSafe(|| rt.block_on(f())));
                let result = match out {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(error)) => Err(LivePlanRunError::Task(error)),
                    Err(payload) => Err(LivePlanRunError::Panicked {
                        payload: LivePlanRunPanicPayload::new(payload),
                    }),
                };
                let _ = done_tx.send(result);
            })
            .map_err(LivePlanRunError::Spawn)?;
        Ok(LivePlanRunHandle {
            result: Some(done_rx),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_worker_stack_is_profile_independent() {
        assert_eq!(LivePlanRunPool::new().stack_size(), 2 * 1024 * 1024);
    }

    #[test]
    fn live_plan_run_panic_includes_str_payload() {
        let payload = LivePlanRunPanicPayload::new(Box::new("teaching block empty"));
        let msg = payload.to_string();
        assert!(
            msg.contains("teaching block empty"),
            "expected panic payload in error; got {msg}"
        );
        assert_eq!(
            *payload.into_inner().downcast::<&str>().unwrap(),
            "teaching block empty"
        );
    }

    #[tokio::test]
    async fn live_plan_run_preserves_non_string_panic_payload() {
        let error = LivePlanRunPool::new()
            .run_local(|| async {
                std::panic::panic_any(42u32);
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("non-string panic payload"));
        let LivePlanRunError::Panicked { payload } = error else {
            panic!("expected worker panic");
        };
        assert_eq!(*payload.into_inner().downcast::<u32>().unwrap(), 42);
    }

    #[tokio::test]
    async fn cancelled_caller_does_not_release_running_worker_permit() {
        let pool = Arc::new(LivePlanRunPool {
            stack_size: live_plan_run_stack_bytes(),
            permits: Arc::new(Semaphore::new(1)),
        });
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let first_pool = Arc::clone(&pool);
        let first = tokio::spawn(async move {
            first_pool
                .run_local(move || async move {
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                })
                .await
        });
        started_rx.await.expect("first worker started");
        first.abort();
        let _ = first.await;
        assert_eq!(pool.permits.available_permits(), 0);

        let (second_started_tx, mut second_started_rx) = oneshot::channel();
        let second_pool = Arc::clone(&pool);
        let second = tokio::spawn(async move {
            second_pool
                .run_local(move || async move {
                    let _ = second_started_tx.send(());
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            second_started_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        release_tx.send(()).expect("first worker still running");
        tokio::time::timeout(std::time::Duration::from_secs(5), second_started_rx)
            .await
            .expect("second worker admitted after first exits")
            .expect("second worker started");
        second.await.unwrap().unwrap();
        let permit =
            tokio::time::timeout(std::time::Duration::from_secs(5), pool.permits.acquire())
                .await
                .expect("worker released its permit")
                .unwrap();
        drop(permit);
    }

    #[tokio::test]
    async fn started_worker_result_survives_a_cancelled_wait() {
        let pool = LivePlanRunPool::new();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let mut handle = pool
            .start_local(move || async move {
                let _ = started.send(());
                let _ = release_rx.await;
                42
            })
            .await
            .unwrap();
        started_rx.await.unwrap();
        tokio::select! {
            _ = handle.wait() => panic!("worker returned before release"),
            _ = tokio::task::yield_now() => {}
        }
        release.send(()).unwrap();
        assert_eq!(handle.wait().await.unwrap(), 42);
        assert!(matches!(
            handle.wait().await,
            Err(LivePlanRunError::ResultDropped)
        ));
    }

    #[test]
    fn panic_execution_failure_projection_preserves_code_and_message() {
        let error = LivePlanRunError::<plasm_runtime::ExecutionFailure>::Panicked {
            payload: LivePlanRunPanicPayload::new(Box::new(String::from("worker fault"))),
        };
        let failure = error.into_execution_failure();
        assert_eq!(failure.code, "live_plan_worker_panicked");
        assert_eq!(failure.diagnostic(), "worker fault");
    }
}
