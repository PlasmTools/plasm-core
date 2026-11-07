//! Session-scoped provider admission; identical service requests share a cache fill.
use plasm_runtime::provider_transport::{
    AttemptRecorder, ProviderResponse, ProviderTransport, ProviderTransportError,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore};
mod metrics;
pub(crate) use metrics::{Phase, PhaseTimer, Workload};

/// Admission is inside the attempt's timeout and lifecycle, not outside it.
pub(crate) struct ScheduledProviderTransport {
    transport: Arc<dyn ProviderTransport>,
    operation: Workload,
}

impl ScheduledProviderTransport {
    pub(crate) fn new(transport: Arc<dyn ProviderTransport>, operation: Workload) -> Self {
        Self {
            transport,
            operation,
        }
    }
}

#[async_trait::async_trait]
impl ProviderTransport for ScheduledProviderTransport {
    async fn send(
        &self,
        body: &str,
        observer: &mut AttemptRecorder<'_>,
    ) -> Result<ProviderResponse, ProviderTransportError> {
        let wait = PhaseTimer::start(self.operation, Phase::PermitWait);
        let _permit = provider_permit().await;
        wait.complete(true);
        let service = PhaseTimer::start(self.operation, Phase::ProviderService);
        let result = self.transport.send(body, observer).await;
        service.complete(result.is_ok());
        result
    }
}

const PROVIDER_CONCURRENCY: usize = 8;
tokio::task_local! { static SESSION_BUDGET: Arc<Semaphore>; }

#[derive(Default)]
pub(crate) struct SessionBudgets(Mutex<BTreeMap<String, Weak<Semaphore>>>);

impl SessionBudgets {
    pub(crate) async fn for_session(&self, session: &str) -> Arc<Semaphore> {
        let mut sessions = self.0.lock().await;
        sessions.retain(|_, budget| budget.strong_count() > 0);
        if let Some(budget) = sessions.get(session).and_then(Weak::upgrade) {
            return budget;
        }
        let budget = Arc::new(Semaphore::new(PROVIDER_CONCURRENCY));
        sessions.insert(session.to_owned(), Arc::downgrade(&budget));
        budget
    }
}

pub(crate) async fn in_session<F: std::future::Future>(
    budget: Arc<Semaphore>,
    future: F,
) -> F::Output {
    SESSION_BUDGET.scope(budget, future).await
}

pub(crate) async fn provider_permit() -> tokio::sync::OwnedSemaphorePermit {
    SESSION_BUDGET
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::new(Semaphore::new(PROVIDER_CONCURRENCY)))
        .acquire_owned()
        .await
        .expect("session discovery semaphore is never closed")
}

#[derive(Default)]
pub(crate) struct DecisionLocks(Mutex<BTreeMap<String, Weak<Mutex<()>>>>);

impl DecisionLocks {
    pub(crate) async fn claim(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut pending = self.0.lock().await;
            pending.retain(|_, lock| lock.strong_count() > 0);
            match pending.get(key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    pending.insert(key.into(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn shared_provider_budget_and_cancellation_release_permits() {
        in_session(Arc::new(Semaphore::new(PROVIDER_CONCURRENCY)), async {
            struct MustNotDispatch;
            #[async_trait::async_trait]
            impl ProviderTransport for MustNotDispatch {
                async fn send(
                    &self,
                    _: &str,
                    _: &mut AttemptRecorder<'_>,
                ) -> Result<ProviderResponse, ProviderTransportError> {
                    panic!("queued request must not dispatch");
                }
            }
            let mut held = Vec::new();
            for _ in 0..PROVIDER_CONCURRENCY {
                held.push(provider_permit().await);
            }
            assert!(provider_permit().now_or_never().is_none());
            let transport = ScheduledProviderTransport::new(
                Arc::new(MustNotDispatch),
                Workload::DiscoveryDecision,
            );
            let mut records = Vec::new();
            let error = plasm_runtime::provider_transport::request_provider(
                &transport,
                "abstract",
                plasm_runtime::provider_transport::ProviderRetryPolicy {
                    attempt_timeout: std::time::Duration::from_millis(20),
                    ..plasm_runtime::provider_transport::ProviderRetryPolicy::discovery_attempt()
                },
                |e| {
                    records.push(e.clone());
                    Ok(())
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                ProviderTransportError::AttemptTimeout {
                    phase: plasm_runtime::provider_transport::AttemptPhase::Admission,
                    status: None,
                    ..
                }
            ));
            assert!(!records.iter().any(|e| matches!(
                e,
                plasm_runtime::provider_transport::ProviderAttempt::Lifecycle {
                    phase: plasm_runtime::provider_transport::AttemptPhase::Dispatch,
                    ..
                }
            )));
            held.pop();
            assert!(provider_permit().now_or_never().is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn separate_sessions_do_not_share_admission() {
        let budgets = SessionBudgets::default();
        let a = budgets.for_session("a").await;
        let a_again = budgets.for_session("a").await;
        let b = budgets.for_session("b").await;
        assert!(Arc::ptr_eq(&a, &a_again));
        assert!(!Arc::ptr_eq(&a, &b));
        let held = a.acquire_many(PROVIDER_CONCURRENCY as u32).await.unwrap();
        assert!(a_again.try_acquire().is_err());
        assert!(b.try_acquire().is_ok());
        drop(held);
    }

    #[tokio::test]
    async fn identical_keys_wait_distinct_keys_progress_and_unused_keys_expire() {
        let locks = DecisionLocks::default();
        let first = locks.claim("same").await;
        assert!(locks.claim("same").now_or_never().is_none());
        let other = locks.claim("other").await;
        drop(first);
        assert!(locks.claim("same").now_or_never().is_some());
        drop(other);
        let _third = locks.claim("third").await;
        assert_eq!(locks.0.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn independent_consumers_share_one_successful_cache_fill() {
        let shared = Arc::new(DecisionLocks::default());
        let cache = Arc::new(Mutex::new(None));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consumer = |locks: Arc<DecisionLocks>| {
            let cache = cache.clone();
            let calls = calls.clone();
            async move {
                let _claim = locks.claim("complete-request-v2").await;
                if let Some(value) = *cache.lock().await {
                    return value;
                }
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::task::yield_now().await;
                *cache.lock().await = Some(7);
                7
            }
        };
        let (left, right) = tokio::join!(consumer(shared.clone()), consumer(shared));
        assert_eq!((left, right), (7, 7));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
