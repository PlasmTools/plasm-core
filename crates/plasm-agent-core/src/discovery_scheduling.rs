//! Process-wide provider admission; identical service requests share a cache fill.
use plasm_runtime::provider_transport::{
    AttemptRecorder, ProviderResponse, ProviderTransport, ProviderTransportError,
};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore, SemaphorePermit};

/// Admission is inside the attempt's timeout and lifecycle, not outside it.
pub(crate) struct ScheduledProviderTransport(pub Arc<dyn ProviderTransport>);

#[async_trait::async_trait]
impl ProviderTransport for ScheduledProviderTransport {
    async fn send(
        &self,
        body: &str,
        observer: &mut AttemptRecorder<'_>,
    ) -> Result<ProviderResponse, ProviderTransportError> {
        let _permit = provider_permit().await;
        self.0.send(body, observer).await
    }
}

const PROVIDER_CONCURRENCY: usize = 8;

pub(crate) async fn provider_permit() -> SemaphorePermit<'static> {
    static BUDGET: OnceLock<Semaphore> = OnceLock::new();
    BUDGET
        .get_or_init(|| Semaphore::new(PROVIDER_CONCURRENCY))
        .acquire()
        .await
        .expect("private process discovery semaphore is never closed")
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
        let transport = ScheduledProviderTransport(Arc::new(MustNotDispatch));
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
