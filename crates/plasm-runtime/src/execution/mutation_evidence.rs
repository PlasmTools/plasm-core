//! Dispatch evidence survives transport errors and interrupted futures. It is
//! separate from semantic operation acknowledgements: a response is not a proof
//! that response decoding, catalog postconditions or the enclosing DAG succeeded.
use super::OperationIdentity;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationDispatchStatus {
    /// Control reached the transport. The server may have committed the write.
    Unresolved,
    /// The transport returned a successful response; later decoding may fail.
    ResponseReceived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationDispatch {
    pub operation: OperationIdentity,
    pub request_fingerprint: String,
    pub status: MutationDispatchStatus,
}

type Observer = Arc<dyn Fn(Vec<MutationDispatch>) + Send + Sync>;

#[derive(Clone, Default)]
pub struct MutationJournal {
    entries: Arc<Mutex<Vec<MutationDispatch>>>,
    observer: Option<Observer>,
}

tokio::task_local! {
    static JOURNAL: MutationJournal;
    static IDENTITY: OperationIdentity;
}

impl MutationJournal {
    /// Observe ordered snapshots synchronously. The callback must be nonblocking
    /// and must not reenter this journal; publication holds its mutation lock.
    pub fn observed(observer: impl Fn(Vec<MutationDispatch>) + Send + Sync + 'static) -> Self {
        Self {
            observer: Some(Arc::new(observer)),
            ..Self::default()
        }
    }

    pub fn snapshot(&self) -> Vec<MutationDispatch> {
        self.entries
            .lock()
            .expect("mutation journal poisoned")
            .clone()
    }

    pub async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        JOURNAL.scope(self.clone(), future).await
    }

    fn change<T>(&self, change: impl FnOnce(&mut Vec<MutationDispatch>) -> T) -> T {
        // Serialize publication with mutation so concurrent completions cannot
        // publish an older snapshot after a newer one.
        let mut entries = self.entries.lock().expect("mutation journal poisoned");
        let result = change(&mut entries);
        if let Some(observer) = &self.observer {
            observer(entries.clone());
        }
        result
    }
}

pub(crate) async fn with_mutation_identity<T>(
    identity: OperationIdentity,
    future: impl Future<Output = T>,
) -> T {
    IDENTITY.scope(identity, future).await
}

pub(crate) struct DispatchReceipt {
    journal: MutationJournal,
    index: usize,
}

impl DispatchReceipt {
    /// Called after authentication and admission, immediately before transport.
    pub(crate) fn begin(request_fingerprint: String) -> Option<Self> {
        let operation = IDENTITY.try_with(Clone::clone).ok()?;
        let journal = JOURNAL.try_with(Clone::clone).ok()?;
        let index = journal.change(|entries| {
            let index = entries.len();
            entries.push(MutationDispatch {
                operation,
                request_fingerprint,
                status: MutationDispatchStatus::Unresolved,
            });
            index
        });
        Some(Self { journal, index })
    }

    pub(crate) fn response_received(self) {
        self.journal.change(|entries| {
            entries[self.index].status = MutationDispatchStatus::ResponseReceived
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> OperationIdentity {
        OperationIdentity {
            entry_id: "fixture".into(),
            capability: "write".into(),
        }
    }

    #[tokio::test]
    async fn journal_scopes_do_not_duplicate_reads_or_nested_writes() {
        let outer = MutationJournal::default();
        let inner = MutationJournal::default();
        outer
            .scope(async {
                assert!(DispatchReceipt::begin("read".into()).is_none());
                inner
                    .scope(with_mutation_identity(identity(), async {
                        let receipt = DispatchReceipt::begin("write".into()).unwrap();
                        assert_eq!(
                            inner.snapshot()[0].status,
                            MutationDispatchStatus::Unresolved
                        );
                        receipt.response_received();
                    }))
                    .await;
            })
            .await;
        assert!(outer.snapshot().is_empty());
        assert_eq!(inner.snapshot().len(), 1);
        assert_eq!(
            inner.snapshot()[0].status,
            MutationDispatchStatus::ResponseReceived
        );
    }

    #[tokio::test]
    async fn interrupted_dispatch_remains_unresolved_without_fabricating_failure() {
        let journal = MutationJournal::default();
        let mut future = Box::pin(journal.scope(with_mutation_identity(identity(), async {
            let _receipt = DispatchReceipt::begin("sent".into()).unwrap();
            std::future::pending::<()>().await;
        })));
        tokio::select! {
            _ = &mut future => panic!("pending dispatch"),
            _ = tokio::task::yield_now() => {},
        }
        drop(future);
        assert_eq!(
            journal.snapshot()[0].status,
            MutationDispatchStatus::Unresolved
        );
        let wire = serde_json::to_value(journal.snapshot()).unwrap();
        assert_eq!(
            serde_json::from_value::<Vec<MutationDispatch>>(wire).unwrap(),
            journal.snapshot()
        );
    }
}
