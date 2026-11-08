//! Dispatch evidence survives transport errors and interrupted futures. It is
//! separate from semantic operation acknowledgements: a response is not a proof
//! that response decoding, catalog postconditions or the enclosing DAG succeeded.
use super::OperationIdentity;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationDispatchStatus {
    /// Control reached the transport. The server may have committed the write.
    Unresolved,
    /// The transport received a response. Status, decoding and effects are separate.
    ResponseReceived,
    /// A catalogue contract matched this particular provider rejection.
    RejectedWithoutEffect,
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
    frozen: Arc<AtomicBool>,
}

/// Holds the journal immutable across one admitted checkpoint.
pub(super) struct FrozenJournal {
    journal: MutationJournal,
    _reservation: super::effect_continuation::SelectionReservation,
}

impl Drop for FrozenJournal {
    fn drop(&mut self) {
        let _entries = self
            .journal
            .entries
            .lock()
            .expect("mutation journal poisoned");
        self.journal.frozen.store(false, Ordering::Release);
    }
}

#[derive(Debug, thiserror::Error)]
#[error("mutation journal is suspended at a checkpoint")]
pub(crate) struct JournalPaused;

tokio::task_local! {
    static JOURNAL: MutationJournal;
    static IDENTITY: OperationIdentity;
    static RECEIPT: Option<DispatchReceipt>;
}

impl MutationJournal {
    pub(super) fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("mutation journal poisoned")
            .len()
    }

    pub(super) fn freeze_retention(
        &self,
        start: usize,
        expected: &[MutationDispatch],
    ) -> Result<FrozenJournal, super::effect_continuation::EffectContinuationError> {
        let entries = self.entries.lock().expect("mutation journal poisoned");
        let candidate = expected.first();
        let stable = matches!(expected, [entry] if entry.status == MutationDispatchStatus::RejectedWithoutEffect)
            && entries.get(start..) == Some(expected)
            && candidate.is_some_and(|candidate| {
                entries[..start].iter().all(|previous| {
                    previous.status == MutationDispatchStatus::RejectedWithoutEffect
                        && previous.operation == candidate.operation
                        && previous.request_fingerprint == candidate.request_fingerprint
                        && previous.resolved_effect == candidate.resolved_effect
                })
            });
        if !stable || self.frozen.load(Ordering::Acquire) {
            return Err(super::effect_continuation::EffectContinuationError::ConcurrentCheckpoint);
        }
        let reservation =
            super::effect_continuation::EffectContinuation::reserve_retained(&*entries)?;
        self.frozen.store(true, Ordering::Release);
        Ok(FrozenJournal {
            journal: self.clone(),
            _reservation: reservation,
        })
    }
    pub(super) fn current() -> Option<Self> {
        JOURNAL.try_with(Clone::clone).ok()
    }

    /// Observe ordered snapshots synchronously. The callback must be nonblocking
    /// and must not reenter this journal; publication holds its mutation lock.
    pub fn observed(observer: impl Fn(Vec<MutationDispatch>) + Send + Sync + 'static) -> Self {
        Self {
            observer: Some(Arc::new(observer)),
            frozen: Arc::new(AtomicBool::new(false)),
            ..Self::default()
        }
    }

    pub fn snapshot(&self) -> Vec<MutationDispatch> {
        self.entries
            .lock()
            .expect("mutation journal poisoned")
            .clone()
    }

    /// Classify only the dispatches made since this attempt began. The shared
    /// journal remains the authority for later failure and progress evidence.
    pub(super) fn classify_attempt(
        &self,
        start: usize,
        error: &crate::RuntimeError,
        contracts: &[plasm_core::schema::NoEffectRejection],
    ) -> Vec<MutationDispatch> {
        let mut entries = self.entries.lock().expect("mutation journal poisoned");
        if start > entries.len() {
            return Vec::new();
        }
        let (prefix, attempt) = entries.split_at_mut(start);
        let before = attempt.to_vec();
        crate::execution_failure::classify_runtime_no_effect_rejection(error, contracts, attempt);
        // A checkpoint may retry only this invocation. An earlier unknown or
        // different dispatch in the same step cannot be hidden by a later
        // known rejection.
        let unsafe_prefix = attempt.first().is_some_and(|candidate| {
            prefix.iter().any(|previous| {
                previous.status != MutationDispatchStatus::RejectedWithoutEffect
                    || previous.operation != candidate.operation
                    || previous.request_fingerprint != candidate.request_fingerprint
                    || previous.resolved_effect != candidate.resolved_effect
            })
        });
        if unsafe_prefix {
            attempt.clone_from_slice(&before);
        }
        let result = attempt.to_vec();
        if result != before {
            if let Some(observer) = &self.observer {
                observer(entries.clone());
            }
        }
        result
    }

    pub async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        JOURNAL.scope(self.clone(), future).await
    }

    #[cfg(test)]
    fn change<T>(&self, change: impl FnOnce(&mut Vec<MutationDispatch>) -> T) -> T {
        match self.try_change(|entries| Ok::<_, std::convert::Infallible>(change(entries))) {
            Ok(result) => result,
            Err(never) => match never {},
        }
    }

    fn try_change<T, E>(
        &self,
        change: impl FnOnce(&mut Vec<MutationDispatch>) -> Result<T, E>,
    ) -> Result<T, E> {
        // Publish under the mutation lock so an older snapshot cannot overtake a newer one.
        let mut entries = self.entries.lock().expect("mutation journal poisoned");
        let result = change(&mut entries)?;
        if let Some(observer) = &self.observer {
            observer(entries.clone());
        }
        Ok(result)
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
    pub(crate) fn begin(request_fingerprint: String) -> Result<Option<Self>, JournalPaused> {
        let Some(operation) = IDENTITY.try_with(Clone::clone).ok() else {
            return Ok(None);
        };
        let Some(journal) = JOURNAL.try_with(Clone::clone).ok() else {
            return Ok(None);
        };
        let index = journal.try_change(|entries| {
            if journal.frozen.load(Ordering::Acquire) {
                return Err(JournalPaused);
            }
            let index = entries.len();
            entries.push(MutationDispatch {
                resolved_effect: super::resolved_effect::current_effect(),
                operation,
                request_fingerprint,
                status: MutationDispatchStatus::Unresolved,
            });
            Ok(index)
        })?;
        Ok(Some(Self { journal, index }))
    }

    pub(crate) fn response_received(&self) {
        let _ = self.journal.try_change(|entries| {
            if entries[self.index].status == MutationDispatchStatus::Unresolved {
                entries[self.index].status = MutationDispatchStatus::ResponseReceived;
                Ok(())
            } else {
                Err(())
            }
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
    async fn checkpoint_freeze_rejects_new_dispatch_and_unfreezes_on_drop() {
        let journal = MutationJournal::default();
        let first = MutationDispatch {
            resolved_effect: None,
            operation: identity(),
            request_fingerprint: "first".into(),
            status: MutationDispatchStatus::RejectedWithoutEffect,
        };
        journal.change(|entries| entries.push(first.clone()));
        let (control, _pauses) = super::super::effect_continuation::EffectContinuation::new_native(
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        control
            .scope(async {
                let frozen = journal
                    .freeze_retention(0, std::slice::from_ref(&first))
                    .unwrap();
                DispatchReceipt {
                    journal: journal.clone(),
                    index: 0,
                }
                .response_received();
                assert_eq!(
                    journal.snapshot()[0].status,
                    MutationDispatchStatus::RejectedWithoutEffect
                );
                journal
                    .scope(with_mutation_identity(identity(), async {
                        assert!(matches!(
                            DispatchReceipt::begin("blocked".into()),
                            Err(JournalPaused)
                        ));
                    }))
                    .await;
                assert_eq!(journal.len(), 1);
                drop(frozen);
                journal
                    .scope(with_mutation_identity(identity(), async {
                        assert!(DispatchReceipt::begin("after".into()).unwrap().is_some());
                    }))
                    .await;
                assert_eq!(journal.len(), 2);
            })
            .await;
    }

    #[tokio::test]
    async fn checkpoint_freeze_rejects_a_journal_that_changed_after_classification() {
        let journal = MutationJournal::default();
        let first = MutationDispatch {
            resolved_effect: None,
            operation: identity(),
            request_fingerprint: "first".into(),
            status: MutationDispatchStatus::RejectedWithoutEffect,
        };
        journal.change(|entries| entries.push(first.clone()));
        journal.change(|entries| {
            entries.push(MutationDispatch {
                status: MutationDispatchStatus::Unresolved,
                ..first.clone()
            })
        });
        let (control, _pauses) = super::super::effect_continuation::EffectContinuation::new_native(
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        control.scope(async {
            assert!(matches!(journal.freeze_retention(0, std::slice::from_ref(&first)),
                Err(super::super::effect_continuation::EffectContinuationError::ConcurrentCheckpoint)));
        }).await;
    }

    #[test]
    fn attempt_classification_never_hides_an_earlier_unknown_dispatch() {
        use plasm_core::schema::{NoEffectRejection, RejectionDetail};
        let rule = NoEffectRejection {
            status: 422,
            detail: RejectionDetail::Exact {
                value: "fixture rejection".into(),
            },
        };
        let mut response =
            crate::HttpStatusFailure::without_request(422, "fixture rejection".into());
        response.mutation_request_fingerprint = Some("fingerprint".into());
        let error = crate::RuntimeError::RequestError {
            source: crate::RequestFailure::HttpStatus(Box::new(response)),
            attempts: 1,
            status: Some(422),
            body: None,
        };
        let dispatch = MutationDispatch {
            resolved_effect: None,
            operation: identity(),
            request_fingerprint: "fingerprint".into(),
            status: MutationDispatchStatus::ResponseReceived,
        };
        let journal = MutationJournal::default();
        journal.change(|entries| entries.extend([dispatch.clone(), dispatch.clone()]));
        let attempt = journal.classify_attempt(1, &error, std::slice::from_ref(&rule));
        assert_eq!(attempt[0].status, MutationDispatchStatus::ResponseReceived);
        journal.change(|entries| entries[0].status = MutationDispatchStatus::RejectedWithoutEffect);
        let attempt = journal.classify_attempt(1, &error, std::slice::from_ref(&rule));
        assert_eq!(
            attempt[0].status,
            MutationDispatchStatus::RejectedWithoutEffect
        );
        journal.change(|entries| {
            entries[0].operation.capability = "other".into();
            entries[1].status = MutationDispatchStatus::ResponseReceived;
        });
        let attempt = journal.classify_attempt(1, &error, &[rule]);
        assert_eq!(attempt[0].status, MutationDispatchStatus::ResponseReceived);
    }

    #[tokio::test]
    async fn received_rejection_survives_error_and_body_interruption() {
        let journal = MutationJournal::default();
        journal
            .scope(with_mutation_identity(identity(), async {
                let receipt = DispatchReceipt::begin("rejected".into()).unwrap();
                let result: Result<(), ()> = with_dispatch_receipt(receipt, async {
                    record_transport_response_received();
                    Err(())
                })
                .await;
                assert!(result.is_err());
                let receipt = DispatchReceipt::begin("body-interrupted".into()).unwrap();
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
                with_dispatch_receipt(DispatchReceipt::begin("request".into()).unwrap(), async {
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
                assert!(DispatchReceipt::begin("read".into()).unwrap().is_none());
                inner
                    .scope(with_mutation_identity(identity(), async {
                        let receipt = DispatchReceipt::begin("write".into()).unwrap().unwrap();
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
            let _receipt = DispatchReceipt::begin("sent".into()).unwrap().unwrap();
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
