//! Review publication protocol: freeze authoritative execution state, snapshot,
//! prepare invisible evidence, seal the fence, then commit without intervening work.
use super::{
    model::*,
    repository::{PendingReview, WorkflowRepository},
    LearningError, WorkflowLearning,
};
use crate::{
    mcp_transport_store::execute_session_registry::{
        ExecuteSessionPersistError, LearningReviewFence,
    },
    server_state::PlasmHostState,
    session_identity::LogicalSessionId,
};

pub(crate) struct ReviewSession {
    pub scope: LibraryScope,
    pub logical_session: LogicalSessionId,
    pub prompt_hash: String,
    pub execute_session: String,
    pub subtasks: Vec<AchievedSubtask>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error(transparent)]
    Repository(#[from] super::repository::RepositoryError),
    #[error(transparent)]
    Gate(#[from] ExecuteSessionPersistError),
    #[error("workflow learning unavailable: {0}")]
    Learning(#[source] Box<LearningError>),
    #[error("review requires a resident session with complete execution evidence")]
    IncompleteSession,
    #[error("review lease expired or execution watermark changed; no review was published")]
    FenceLost,
    #[error("review sealing timed out; retry when the session is idle")]
    Deadline,
}

type Result<T> = std::result::Result<T, ReviewError>;

trait ReviewCommit {
    fn handle(&self) -> &ReviewHandle;
    async fn commit(self) -> Result<ReviewHandle>;
}
trait ReviewJournal {
    type Pending<'a>: ReviewCommit
    where
        Self: 'a;
    async fn prepare<'a>(&'a self, request: &ReviewRequest) -> Result<Self::Pending<'a>>;
}
trait ReviewGate {
    type Fence;
    async fn begin(&self) -> Result<Self::Fence>;
    async fn snapshot(&self) -> Result<ReviewRequest>;
    async fn seal(&self, fence: &Self::Fence, handle: &ReviewHandle) -> Result<bool>;
    async fn abort(&self, fence: &Self::Fence) -> Result<()>;
}

impl ReviewCommit for PendingReview<'_> {
    fn handle(&self) -> &ReviewHandle {
        &self.handle
    }
    async fn commit(self) -> Result<ReviewHandle> {
        Ok(PendingReview::commit(self).await?)
    }
}
impl ReviewJournal for WorkflowRepository {
    type Pending<'a> = PendingReview<'a>;
    async fn prepare<'a>(&'a self, request: &ReviewRequest) -> Result<Self::Pending<'a>> {
        Ok(self.prepare_review(request).await?)
    }
}

struct SessionReviewGate<'a> {
    host: &'a PlasmHostState,
    request: ReviewSession,
}
impl ReviewGate for SessionReviewGate<'_> {
    type Fence = LearningReviewFence;
    async fn begin(&self) -> Result<Self::Fence> {
        Ok(self
            .host
            .execute_session_registry
            .begin_learning_review(&self.request.prompt_hash, &self.request.execute_session)
            .await?)
    }
    async fn snapshot(&self) -> Result<ReviewRequest> {
        // Re-read only after the authoritative gate prevents new effects.
        let session = self
            .host
            .sessions
            .get_by_strs(&self.request.prompt_hash, &self.request.execute_session)
            .await
            .ok_or(ReviewError::IncompleteSession)?;
        let logical = self
            .host
            .logical_sessions
            .get(self.request.logical_session)
            .await
            .ok_or(ReviewError::IncompleteSession)?;
        if !session.open_live_operation_handles().is_empty() {
            return Err(ReviewError::IncompleteSession);
        }
        Ok(ReviewRequest {
            scope: self.request.scope.clone(),
            logical_session: self.request.logical_session,
            session_revision: session.core.tip_seq().await.0,
            intent: logical.accumulated_intent,
            subtasks: self.request.subtasks.clone(),
        })
    }
    async fn seal(&self, fence: &Self::Fence, handle: &ReviewHandle) -> Result<bool> {
        Ok(self
            .host
            .execute_session_registry
            .seal_learning_review(
                &self.request.prompt_hash,
                &self.request.execute_session,
                fence,
                &handle.id.to_string(),
                &handle.digest,
            )
            .await?)
    }
    async fn abort(&self, fence: &Self::Fence) -> Result<()> {
        Ok(self
            .host
            .execute_session_registry
            .abort_learning_review(
                &self.request.prompt_hash,
                &self.request.execute_session,
                fence,
            )
            .await?)
    }
}

async fn publish_review(
    journal: &impl ReviewJournal,
    gate: &impl ReviewGate,
) -> Result<ReviewHandle> {
    let fence = gate.begin().await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let snapshot = gate.snapshot().await?;
        let pending = journal.prepare(&snapshot).await?;
        if !gate.seal(&fence, pending.handle()).await? {
            return Err(ReviewError::FenceLost);
        }
        pending.commit().await
    })
    .await
    .unwrap_or(Err(ReviewError::Deadline));
    if result.is_err() {
        if let Err(cleanup) = gate.abort(&fence).await {
            tracing::warn!(%cleanup,"review lease cleanup unavailable; lease will expire");
        }
    }
    result
}

impl WorkflowLearning {
    pub(crate) async fn seal_review(
        &self,
        host: &PlasmHostState,
        request: ReviewSession,
    ) -> Result<ReviewHandle> {
        let repository = self
            .repository()
            .await
            .map_err(|error| ReviewError::Learning(Box::new(error)))?;
        publish_review(repository.as_ref(), &SessionReviewGate { host, request }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::test_identities as ids;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        None,
        Prepare,
        ExpiredFence,
        Commit,
        Abort,
    }
    struct Memory {
        events: Mutex<Vec<&'static str>>,
        visible: AtomicBool,
        fault: Fault,
    }
    impl Memory {
        fn event(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
        }
    }
    struct Pending {
        memory: Arc<Memory>,
        handle: ReviewHandle,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if !self.memory.visible.load(Ordering::SeqCst) {
                self.memory.event("rollback");
            }
        }
    }
    impl ReviewCommit for Pending {
        fn handle(&self) -> &ReviewHandle {
            &self.handle
        }
        async fn commit(self) -> Result<ReviewHandle> {
            self.memory.event("commit");
            if self.memory.fault == Fault::Commit {
                return Err(ReviewError::IncompleteSession);
            }
            self.memory.visible.store(true, Ordering::SeqCst);
            Ok(self.handle.clone())
        }
    }
    impl ReviewJournal for Arc<Memory> {
        type Pending<'a> = Pending;
        async fn prepare<'a>(&'a self, _: &ReviewRequest) -> Result<Self::Pending<'a>> {
            self.event("prepare");
            if matches!(self.fault, Fault::Prepare | Fault::Abort) {
                return Err(ReviewError::IncompleteSession);
            }
            Ok(Pending {
                memory: self.clone(),
                handle: ReviewHandle {
                    id: uuid::Uuid::new_v4(),
                    digest: "sealed".into(),
                },
            })
        }
    }
    impl ReviewGate for Arc<Memory> {
        type Fence = ();
        async fn begin(&self) -> Result<()> {
            self.event("begin");
            Ok(())
        }
        async fn snapshot(&self) -> Result<ReviewRequest> {
            self.event("snapshot");
            Ok(ReviewRequest {
                scope: LibraryScope {
                    tenant: "t".into(),
                    project: "p".into(),
                },
                logical_session: ids::logical("s"),
                session_revision: 1,
                intent: "read".into(),
                subtasks: vec![],
            })
        }
        async fn seal(&self, _: &(), _: &ReviewHandle) -> Result<bool> {
            self.event("seal");
            Ok(self.fault != Fault::ExpiredFence)
        }
        async fn abort(&self, _: &()) -> Result<()> {
            self.event("abort");
            if self.fault == Fault::Abort {
                Err(ReviewError::IncompleteSession)
            } else {
                Ok(())
            }
        }
    }
    fn memory(fault: Fault) -> Arc<Memory> {
        Arc::new(Memory {
            events: Mutex::new(vec![]),
            visible: AtomicBool::new(false),
            fault,
        })
    }
    #[tokio::test]
    async fn publication_order_is_gate_snapshot_prepare_seal_commit() {
        let memory = memory(Fault::None);
        publish_review(&memory, &memory).await.unwrap();
        assert!(memory.visible.load(Ordering::SeqCst));
        assert_eq!(
            *memory.events.lock().unwrap(),
            vec!["begin", "snapshot", "prepare", "seal", "commit"]
        );
    }
    #[tokio::test]
    async fn failed_prepare_expired_fence_and_commit_never_publish() {
        for fault in [
            Fault::Prepare,
            Fault::ExpiredFence,
            Fault::Commit,
            Fault::Abort,
        ] {
            let memory = memory(fault);
            assert!(publish_review(&memory, &memory).await.is_err());
            assert!(!memory.visible.load(Ordering::SeqCst));
            let events = memory.events.lock().unwrap();
            assert_eq!(events.last(), Some(&"abort"));
            if fault == Fault::ExpiredFence {
                assert!(!events.contains(&"commit"));
                assert!(events.contains(&"rollback"));
            }
        }
    }
}
