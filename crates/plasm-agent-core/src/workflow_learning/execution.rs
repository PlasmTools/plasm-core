//! Evidence follows the operation lifetime, independently of transport wait/cancel.

use super::{model::LibraryScope, CompletedExecution, LearningError};
use crate::execute_session::ExecuteSession;
use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::server_state::PlasmHostState;
use crate::session_identity::LogicalSessionId;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecutionLearningError {
    #[error("execution evidence watermark unavailable")]
    Watermark(#[from] ExecuteSessionPersistError),
    #[error("execution evidence journal unavailable")]
    Journal(#[from] LearningError),
}

/// An unavailable journal cannot acknowledge the durable execution watermark.
pub(crate) enum JournalAdmission<P> {
    Recording(P),
    Unrecordable,
}

/// Admission consumes the unused journal and yields an established checkpoint.
pub(crate) trait EvidenceJournal {
    type Error: std::error::Error;
    type Pending: PendingEvidenceJournal<Error = Self::Error>;
    async fn begin(self) -> Result<JournalAdmission<Self::Pending>, Self::Error>;
}

/// Only an established checkpoint can receive evidence. Terminal calls consume
/// it; failed persistence leaves the authoritative watermark unresolved.
pub(crate) trait PendingEvidenceJournal {
    type Error: std::error::Error;
    type Evidence<'a>;
    async fn record<'e>(&self, evidence: Self::Evidence<'e>) -> Result<(), Self::Error>;
    async fn complete(self) -> Result<(), Self::Error>;
    async fn fail(self) -> Result<(), Self::Error>;
    fn excluded(_error: &Self::Error) -> bool {
        false
    }
    async fn discard(self) -> Result<(), Self::Error>
    where
        Self: Sized,
    {
        self.fail().await
    }
}

/// Dropping a ticket (panic, worker loss, cancellation) leaves the durable
/// checkpoint pending. Its private state cannot acknowledge another attempt.
pub(crate) struct ExecutionLearningTicket<P: PendingEvidenceJournal> {
    admission: JournalAdmission<P>,
}

impl<P: PendingEvidenceJournal> ExecutionLearningTicket<P> {
    pub(crate) async fn begin<J: EvidenceJournal<Pending = P>>(
        journal: J,
    ) -> Result<Self, J::Error> {
        Ok(Self {
            admission: journal.begin().await?,
        })
    }

    pub(crate) async fn complete(self, evidence: P::Evidence<'_>) -> Result<(), P::Error> {
        let JournalAdmission::Recording(pending) = self.admission else {
            return Ok(());
        };
        if let Err(error) = pending.record(evidence).await {
            if P::excluded(&error) {
                return pending.discard().await;
            }
            return Err(error);
        }
        pending.complete().await
    }

    pub(crate) async fn fail(self) -> Result<(), P::Error> {
        match self.admission {
            JournalAdmission::Recording(pending) => pending.fail().await,
            JournalAdmission::Unrecordable => Ok(()),
        }
    }
}

pub(crate) struct CompletedProgram<'a> {
    pub(crate) bundle: &'a PlasmCompBundle,
    pub(crate) output: &'a PlasmPlanRunResult,
}

struct ExecutionAttemptId(uuid::Uuid);
struct ExecutionWatermark(u64);

pub(crate) struct PostgresEvidenceJournal<'a> {
    host: &'a PlasmHostState,
    session: &'a ExecuteSession,
    execute_session: &'a str,
    logical: LogicalSessionId,
    attempt: ExecutionAttemptId,
}

pub(crate) struct PendingPostgresEvidenceJournal<'a> {
    context: PostgresEvidenceJournal<'a>,
    scope: LibraryScope,
    watermark: ExecutionWatermark,
    training_permit: super::repository::training::TrainingEpochPermit,
}

impl<'a> PostgresEvidenceJournal<'a> {
    pub(crate) async fn for_operation(
        host: &'a PlasmHostState,
        session: &'a ExecuteSession,
        execute_session: &'a str,
    ) -> Option<Self> {
        if !host.workflow_learning.enabled() {
            return None;
        }
        let logical = LogicalSessionId(
            host.logical_session_id_for_execute_binding(&session.prompt_hash, execute_session)
                .await?,
        );
        Some(Self {
            host,
            session,
            execute_session,
            logical,
            attempt: ExecutionAttemptId(uuid::Uuid::new_v4()),
        })
    }
}

impl<'a> EvidenceJournal for PostgresEvidenceJournal<'a> {
    type Error = ExecutionLearningError;
    type Pending = PendingPostgresEvidenceJournal<'a>;

    async fn begin(self) -> Result<JournalAdmission<Self::Pending>, Self::Error> {
        let attempt = self.attempt.0.to_string();
        let Some((scope, training_permit)) = self
            .host
            .workflow_learning
            .begin_execution(&self.session.tenant_scope, self.logical, &attempt)
            .await?
        else {
            return Ok(JournalAdmission::Unrecordable);
        };
        let watermark = match self
            .host
            .execute_session_registry
            .begin_learning_execution(&self.session.prompt_hash, self.execute_session, &attempt)
            .await
        {
            Ok(watermark) => ExecutionWatermark(watermark),
            Err(error) => {
                return Err(error.into());
            }
        };
        Ok(JournalAdmission::Recording(
            PendingPostgresEvidenceJournal {
                context: self,
                scope,
                watermark,
                training_permit,
            },
        ))
    }
}

impl PendingEvidenceJournal for PendingPostgresEvidenceJournal<'_> {
    type Error = ExecutionLearningError;
    type Evidence<'a> = CompletedProgram<'a>;

    async fn record<'e>(&self, evidence: Self::Evidence<'e>) -> Result<(), Self::Error> {
        let CompletedProgram { bundle, output } = evidence;
        let context = &self.context;
        context
            .host
            .workflow_learning
            .record_completed(CompletedExecution {
                session: context.session,
                logical_session: context.logical,
                execute_session: context.execute_session,
                source: bundle.source().unwrap_or_default(),
                bundle,
                output,
                training_permit: &self.training_permit,
                execution_id: &context.attempt.0.to_string(),
            })
            .await?;
        Ok(())
    }

    async fn complete(self) -> Result<(), Self::Error> {
        let context = &self.context;
        context
            .host
            .execute_session_registry
            .complete_learning_execution(
                &context.session.prompt_hash,
                context.execute_session,
                self.watermark.0,
            )
            .await?;
        Ok(())
    }

    async fn fail(self) -> Result<(), Self::Error> {
        let context = &self.context;
        context
            .host
            .workflow_learning
            .finish_execution(
                &self.scope,
                context.logical,
                &context.attempt.0.to_string(),
                false,
            )
            .await
            .map_err(ExecutionLearningError::from)
    }

    fn excluded(error: &Self::Error) -> bool {
        matches!(
            error,
            ExecutionLearningError::Journal(LearningError::Repository(
                super::repository::RepositoryError::Training(
                    super::repository::training::TrainingError::Inactive
                        | super::repository::training::TrainingError::Held
                        | super::repository::training::TrainingError::GrantUnavailable
                )
            ))
        )
    }

    async fn discard(self) -> Result<(), Self::Error> {
        let context = &self.context;
        context
            .host
            .workflow_learning
            .repository()
            .await?
            .discard_execution(&self.scope, context.logical, &context.attempt.0.to_string())
            .await
            .map_err(LearningError::from)?;
        context
            .host
            .execute_session_registry
            .complete_learning_execution(
                &context.session.prompt_hash,
                context.execute_session,
                self.watermark.0,
            )
            .await?;
        Ok(())
    }
}

/// One evidence protocol for server operations and native engine execution.
pub async fn observe_execution<F>(
    host: &PlasmHostState,
    session: &ExecuteSession,
    execute_session: &str,
    bundle: &PlasmCompBundle,
    execution: F,
) -> Result<PlasmPlanRunResult, plasm_runtime::ExecutionFailure>
where
    F: std::future::Future<Output = Result<PlasmPlanRunResult, plasm_runtime::ExecutionFailure>>,
{
    if bundle.execution_evidence_kind()
        == crate::plasm_comp_bundle::ExecutionEvidenceKind::ResultDelivery
    {
        return execution.await;
    }
    let ticket = match PostgresEvidenceJournal::for_operation(host, session, execute_session).await
    {
        Some(journal) => match ExecutionLearningTicket::begin(journal).await {
            Ok(ticket) => Some(ticket),
            Err(error) => {
                tracing::warn!(target: "plasm_agent::workflow_learning", %error,
                    "training capture unavailable; ordinary execution continues");
                None
            }
        },
        None => None,
    };
    let result = execution.await;
    if let Some(ticket) = ticket {
        let evidence = match &result {
            Ok(output) => ticket.complete(CompletedProgram { bundle, output }).await,
            Err(_) => ticket.fail().await,
        };
        if let Err(error) = evidence {
            tracing::warn!(target: "plasm_agent::workflow_learning", %error, "operation evidence remains unresolved");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
    #[error("injected evidence fault at {0}")]
    struct Fault(Stage);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Stage {
        Begin,
        Record,
        Complete,
        Fail,
    }
    impl std::fmt::Display for Stage {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::Begin => "begin",
                Self::Record => "record",
                Self::Complete => "complete",
                Self::Fail => "fail",
            })
        }
    }

    #[derive(Default)]
    struct Memory {
        events: RefCell<Vec<Stage>>,
        pending: Cell<bool>,
    }

    struct Journal {
        memory: Rc<Memory>,
        fault: Option<Stage>,
        unrecordable: bool,
    }

    impl Journal {
        fn event(&self, stage: Stage) -> Result<(), Fault> {
            self.memory.events.borrow_mut().push(stage);
            if self.fault == Some(stage) {
                Err(Fault(stage))
            } else {
                Ok(())
            }
        }
    }

    struct PendingJournal(Journal);

    impl EvidenceJournal for Journal {
        type Error = Fault;
        type Pending = PendingJournal;

        async fn begin(self) -> Result<JournalAdmission<Self::Pending>, Fault> {
            self.event(Stage::Begin)?;
            self.memory.pending.set(true);
            Ok(if self.unrecordable {
                JournalAdmission::Unrecordable
            } else {
                JournalAdmission::Recording(PendingJournal(self))
            })
        }
    }

    impl PendingEvidenceJournal for PendingJournal {
        type Error = Fault;
        type Evidence<'a> = ();
        async fn record<'e>(&self, (): Self::Evidence<'e>) -> Result<(), Fault> {
            self.0.event(Stage::Record)
        }
        async fn complete(self) -> Result<(), Fault> {
            self.0.event(Stage::Complete)?;
            self.0.memory.pending.set(false);
            Ok(())
        }
        async fn fail(self) -> Result<(), Fault> {
            self.0.event(Stage::Fail)
        }
    }

    fn journal(fault: Option<Stage>, unrecordable: bool) -> (Journal, Rc<Memory>) {
        let memory = Rc::new(Memory::default());
        (
            Journal {
                memory: Rc::clone(&memory),
                fault,
                unrecordable,
            },
            memory,
        )
    }

    #[tokio::test]
    async fn completed_operation_records_before_acknowledging() {
        let (journal, memory) = journal(None, false);
        ExecutionLearningTicket::begin(journal)
            .await
            .unwrap()
            .complete(())
            .await
            .unwrap();
        assert_eq!(
            *memory.events.borrow(),
            [Stage::Begin, Stage::Record, Stage::Complete]
        );
        assert!(!memory.pending.get());
    }

    #[tokio::test]
    async fn persistence_fault_never_acknowledges_completion() {
        let (journal, memory) = journal(Some(Stage::Record), false);
        assert!(ExecutionLearningTicket::begin(journal)
            .await
            .unwrap()
            .complete(())
            .await
            .is_err());
        assert_eq!(*memory.events.borrow(), [Stage::Begin, Stage::Record]);
        assert!(memory.pending.get());
    }

    #[tokio::test]
    async fn checkpoint_fault_retains_unresolved_watermark() {
        let (journal, memory) = journal(Some(Stage::Complete), false);
        assert!(ExecutionLearningTicket::begin(journal)
            .await
            .unwrap()
            .complete(())
            .await
            .is_err());
        assert_eq!(
            *memory.events.borrow(),
            [Stage::Begin, Stage::Record, Stage::Complete]
        );
        assert!(memory.pending.get());
    }

    #[tokio::test]
    async fn interrupted_or_failed_operation_never_records_success() {
        let (journal, memory) = journal(None, false);
        drop(ExecutionLearningTicket::begin(journal).await.unwrap());
        assert_eq!(*memory.events.borrow(), [Stage::Begin]);
        assert!(memory.pending.get());
        let (journal, memory) = self::journal(None, false);
        ExecutionLearningTicket::begin(journal)
            .await
            .unwrap()
            .fail()
            .await
            .unwrap();
        assert_eq!(*memory.events.borrow(), [Stage::Begin, Stage::Fail]);
        assert!(memory.pending.get());
    }

    #[tokio::test]
    async fn unavailable_journal_cannot_become_recordable_at_completion() {
        let (journal, memory) = journal(None, true);
        ExecutionLearningTicket::begin(journal)
            .await
            .unwrap()
            .complete(())
            .await
            .unwrap();
        assert_eq!(*memory.events.borrow(), [Stage::Begin]);
        assert!(memory.pending.get());
    }

    #[tokio::test]
    async fn failed_initial_watermark_cannot_issue_ticket() {
        let (journal, memory) = journal(Some(Stage::Begin), false);
        assert!(ExecutionLearningTicket::begin(journal).await.is_err());
        assert_eq!(*memory.events.borrow(), [Stage::Begin]);
        assert!(!memory.pending.get());
    }
}
