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
    /// The transport received a response. Status, decoding and effects are separate.
    ResponseReceived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationDispatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_effect: Option<super::ResolvedEffectEvidence>,
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
    static RECEIPT: Option<DispatchReceipt>;
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

#[derive(Clone)]
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
                resolved_effect: super::resolved_effect::current_effect(),
                operation,
                request_fingerprint,
                status: MutationDispatchStatus::Unresolved,
            });
            index
        });
        Some(Self { journal, index })
    }

    pub(crate) fn response_received(&self) {
        self.journal.change(|entries| {
            entries[self.index].status = MutationDispatchStatus::ResponseReceived
        });
    }
}

pub(crate) async fn with_dispatch_receipt<T>(
    receipt: Option<DispatchReceipt>,
    future: impl Future<Output = T>,
) -> T {
    RECEIPT.scope(receipt, future).await
}

/// Called by transports at receipt, before status classification or body decoding.
pub fn record_transport_response_received() {
    let _ = RECEIPT.try_with(|receipt| {
        if let Some(receipt) = receipt {
            receipt.response_received();
        }
    });
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
    async fn received_rejection_survives_error_and_body_interruption() {
        let journal = MutationJournal::default();
        journal
            .scope(with_mutation_identity(identity(), async {
                let receipt = DispatchReceipt::begin("rejected".into());
                let result: Result<(), ()> = with_dispatch_receipt(receipt, async {
                    record_transport_response_received();
                    Err(())
                })
                .await;
                assert!(result.is_err());
                let receipt = DispatchReceipt::begin("body-interrupted".into());
                let mut body = Box::pin(with_dispatch_receipt(receipt, async {
                    record_transport_response_received();
                    std::future::pending::<()>().await;
                }));
                tokio::select! {
                    _ = &mut body => panic!("pending body"),
                    _ = tokio::task::yield_now() => {},
                }
                drop(body);
            }))
            .await;
        assert_eq!(journal.snapshot().len(), 2);
        assert!(journal
            .snapshot()
            .iter()
            .all(|entry| entry.status == MutationDispatchStatus::ResponseReceived));
    }

    #[tokio::test]
    async fn http_response_receipt_precedes_failed_body_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let received = stream.read(&mut request).await.unwrap();
            assert!(
                received > 0,
                "client must begin the request before response"
            );
            stream.write_all(b"HTTP/1.1 422 Unprocessable Entity\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}").await.unwrap();
            stream.shutdown().await.unwrap();
        });
        let journal = MutationJournal::default();
        journal
            .scope(with_mutation_identity(identity(), async {
                with_dispatch_receipt(DispatchReceipt::begin("request".into()), async {
                    let response = reqwest::Client::new()
                        .get(format!("http://{address}"))
                        .send()
                        .await
                        .unwrap();
                    assert!(crate::http_transport::read_http_response(response, "GET")
                        .await
                        .is_err());
                })
                .await;
            }))
            .await;
        server.await.unwrap();
        assert_eq!(
            journal.snapshot()[0].status,
            MutationDispatchStatus::ResponseReceived
        );
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
