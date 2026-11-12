//! Validated decision envelopes share ownership through durable completion.
use crate::decision_codec::DecisionPacket;
use crate::discovery_matcher::{self as matcher, DecisionProviderError};
use crate::discovery_scheduling::DecisionLocks;
use crate::discovery_store::DiscoveryStore;
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

#[derive(Debug, thiserror::Error)]
#[error("a received discovery decision must be persisted before receiving another")]
struct UnpersistedDecision;

#[async_trait::async_trait]
pub(crate) trait DecisionCache: Sync {
    type Owner: Send;
    async fn claim(&self, key: &str) -> Result<Self::Owner, DecisionProviderError>;
    async fn load(&self, key: &str) -> Result<Option<String>, DecisionProviderError>;
    async fn save(
        &self,
        key: &str,
        raw: &str,
        owner: &mut Self::Owner,
    ) -> Result<(), DecisionProviderError>;
}

#[async_trait::async_trait]
impl DecisionCache for DiscoveryStore {
    type Owner = crate::discovery_store::DecisionLease;

    async fn claim(&self, key: &str) -> Result<Self::Owner, DecisionProviderError> {
        Ok(self.searches().claim_decision(key).await?)
    }

    async fn load(&self, key: &str) -> Result<Option<String>, DecisionProviderError> {
        Ok(self.searches().decision(key).await?)
    }

    async fn save(
        &self,
        key: &str,
        raw: &str,
        owner: &mut Self::Owner,
    ) -> Result<(), DecisionProviderError> {
        Ok(owner.persist(key, raw).await?)
    }
}

pub(crate) struct DecisionCheckpoint<'a, C: DecisionCache> {
    cache: &'a C,
    locks: &'a DecisionLocks,
    pending: Mutex<Option<PendingEnvelope<C::Owner>>>,
}

struct PendingEnvelope<O> {
    key: String,
    raw: String,
    // Retain cache-fill ownership until the write completes, including failures
    // and cancellation. Another search must not duplicate the in-flight fill.
    _owner: OwnedMutexGuard<()>,
    durable_owner: O,
}

impl<'a, C: DecisionCache> DecisionCheckpoint<'a, C> {
    pub(crate) fn new(cache: &'a C, locks: &'a DecisionLocks) -> Self {
        Self {
            cache,
            locks,
            pending: Mutex::new(None),
        }
    }

    pub(crate) async fn receive<P, F, Fut>(
        &self,
        issued: P,
        fetch: F,
    ) -> Result<String, DecisionProviderError>
    where
        P: DecisionPacket,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    {
        // Reserve the slot before claiming a fill lock: an outstanding decision
        // may own that same lock, so checking afterwards could deadlock.
        let mut pending = self.vacant_slot().await?;
        let owner = self.locks.claim(issued.cache_key()).await;
        let durable_owner = self.cache.claim(issued.cache_key()).await?;
        if let Some(raw) = self.cache.load(issued.cache_key()).await? {
            issued
                .validate_response(&raw)
                .map_err(|error| matcher::online::ReceivedResponseFailure::new(&raw, error))?;
            return Ok(raw);
        }
        let raw = fetch().await?;
        issued
            .validate_response(&raw)
            .map_err(|error| matcher::online::ReceivedResponseFailure::new(&raw, error))?;
        *pending = Some(PendingEnvelope {
            key: issued.cache_key().into(),
            raw: raw.clone(),
            _owner: owner,
            durable_owner,
        });
        Ok(raw)
    }

    async fn vacant_slot(
        &self,
    ) -> Result<MutexGuard<'_, Option<PendingEnvelope<C::Owner>>>, DecisionProviderError> {
        let pending = self.pending.lock().await;
        if pending.is_some() {
            return Err(Box::new(UnpersistedDecision));
        }
        Ok(pending)
    }

    pub(crate) async fn persist(&self) -> Result<(), DecisionProviderError> {
        // Receipt persistence is part of the durable search checkpoint, outside
        // the provider deadline. Never report success before this write succeeds.
        // Holding the guard through the await also makes cancellation release it.
        if let Some(mut pending) = self.pending.lock().await.take() {
            let started = std::time::Instant::now();
            let result = self
                .cache
                .save(&pending.key, &pending.raw, &mut pending.durable_owner)
                .await;
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                success = result.is_ok(),
                "discovery decision checkpoint completed"
            );
            result?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    struct StalledCache;

    #[async_trait::async_trait]
    impl DecisionCache for StalledCache {
        type Owner = ();
        async fn claim(&self, _: &str) -> Result<Self::Owner, DecisionProviderError> {
            Ok(())
        }
        async fn load(&self, _: &str) -> Result<Option<String>, DecisionProviderError> {
            Ok(None)
        }

        async fn save(
            &self,
            _: &str,
            _: &str,
            _: &mut Self::Owner,
        ) -> Result<(), DecisionProviderError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn outstanding_decision_is_a_typed_error_without_losing_fill_ownership() {
        let locks = DecisionLocks::default();
        let checkpoint = DecisionCheckpoint::new(&StalledCache, &locks);
        *checkpoint.pending.lock().await = Some(PendingEnvelope {
            key: "abstract".into(),
            raw: "validated response".into(),
            _owner: locks.claim("abstract").await,
            durable_owner: (),
        });
        let Err(error) = checkpoint.vacant_slot().await else {
            panic!("unpersisted decision must retain its slot");
        };
        assert!(error.downcast_ref::<UnpersistedDecision>().is_some());
        assert!(locks.claim("abstract").now_or_never().is_none());
        assert!(checkpoint.pending.lock().await.is_some());
    }

    #[tokio::test]
    async fn cancelled_checkpoint_releases_fill_ownership_without_retrying_write() {
        let locks = DecisionLocks::default();
        let checkpoint = DecisionCheckpoint::new(&StalledCache, &locks);
        *checkpoint.pending.lock().await = Some(PendingEnvelope {
            key: "abstract".into(),
            raw: "validated response".into(),
            _owner: locks.claim("abstract").await,
            durable_owner: (),
        });
        {
            let write = checkpoint.persist();
            tokio::pin!(write);
            assert!(write.as_mut().now_or_never().is_none());
            assert!(locks.claim("abstract").now_or_never().is_none());
        } // Dropping the pending write must release ownership.
        assert!(locks.claim("abstract").now_or_never().is_some());
        assert!(checkpoint.pending.lock().await.is_none());
        assert!(checkpoint.persist().now_or_never().unwrap().is_ok());
    }
}
