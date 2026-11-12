//! Shared opt-in training authority and durable session lifecycle.
//!
//! Transport adapters construct actors only from authenticated context. No actor
//! or authorization proof implements `Deserialize`.

use super::WorkflowRepository;
use crate::workflow_learning::model::*;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedTrainingActor {
    scope: LibraryScope,
    subject: String,
    credential: String,
    authority: ActorAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ActorAuthority {
    Caller,
    HumanOwner,
    TrustedHost,
}

impl TrustedTrainingActor {
    /// The adapter must have already authenticated subject and credential.
    pub fn authenticated(scope: LibraryScope, subject: String, credential: String) -> Result<Self> {
        Self::new(scope, subject, credential, ActorAuthority::Caller)
    }

    /// For verified owner controls only, never MCP model-callable input.
    pub fn human_owner(scope: LibraryScope, subject: String, credential: String) -> Result<Self> {
        Self::new(scope, subject, credential, ActorAuthority::HumanOwner)
    }

    /// Explicit embedded-host configuration, never tool-call arguments.
    pub fn trusted_host(scope: LibraryScope, subject: String, credential: String) -> Result<Self> {
        Self::new(scope, subject, credential, ActorAuthority::TrustedHost)
    }

    fn new(
        scope: LibraryScope,
        subject: String,
        credential: String,
        authority: ActorAuthority,
    ) -> Result<Self> {
        if [
            scope.tenant.as_str(),
            scope.project.as_str(),
            &subject,
            &credential,
        ]
        .iter()
        .any(|v| v.trim().is_empty())
        {
            return Err(TrainingError::InvalidCommand);
        }
        Ok(Self {
            scope,
            subject,
            credential,
            authority,
        })
    }

    pub fn scope(&self) -> &LibraryScope {
        &self.scope
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn credential(&self) -> &str {
        &self.credential
    }
    pub fn is_human_owner(&self) -> bool {
        self.authority == ActorAuthority::HumanOwner
    }
    pub fn may_administer(&self) -> bool {
        self.authority != ActorAuthority::Caller
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingAction {
    Capture,
    Review,
    PublishAutomatically,
    Release,
    /// Explicit project-scoped authority to attest an imported review.
    AttestImportedReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum PublicationPolicy {
    #[default]
    HumanReviewed,
    DelegatedAutomatic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingGrantSpec {
    pub grantee: String,
    pub credential: String,
    pub actions: Vec<TrainingAction>,
    pub expires_at: DateTime<Utc>,
    pub publication_policy: PublicationPolicy,
    pub target: TrainingGrantTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum TrainingGrantTarget {
    Project,
    Session { logical_session: LogicalSessionId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingGrant {
    pub id: Uuid,
    pub scope: LibraryScope,
    pub issuer: String,
    pub grantee: String,
    pub credential: String,
    pub actions: Vec<TrainingAction>,
    pub publication_policy: PublicationPolicy,
    pub revision: u64,
    pub expires_at: DateTime<Utc>,
    pub target: TrainingGrantTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TrainingState {
    #[default]
    Ordinary,
    AwaitingAuthorization {
        request_ref: Uuid,
        expires_at: DateTime<Utc>,
    },
    Active {
        epoch_ref: Uuid,
        grant_ref: Uuid,
        start_revision: u64,
        publication_policy: PublicationPolicy,
    },
    Stopped {
        reason: TrainingStopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingStopReason {
    Requested,
    Declined,
    GrantUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum CaptureAvailability {
    Available,
    Unavailable { reason: CaptureUnavailableReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureUnavailableReason {
    GrantUnavailable,
    PersistenceUnavailable,
    EvaluationSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingSessionPurpose {
    Operational,
    Evaluation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingStatus {
    pub logical_session: LogicalSessionId,
    pub revision: u64,
    pub training_state: TrainingState,
    pub capture_availability: CaptureAvailability,
    pub held: bool,
    pub session_purpose: TrainingSessionPurpose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[serde(from = "TrainingCommandWire")]
pub enum TrainingCommand {
    Request,
    Stop,
    Resume,
    AuthorizeController,
    ResolveRequest {
        request_ref: Uuid,
        request_revision: u64,
        approve: bool,
    },
    RequestCandidateDecision {
        selection: Vec<TrainingCandidateSelection>,
    },
    ReleaseHeld {
        review_refs: Vec<Uuid>,
    },
    RequestRelease {
        review_refs: Vec<Uuid>,
    },
    ResolveRelease {
        request_ref: Uuid,
        request_revision: u64,
        approve: bool,
    },
    ReleaseExecutions {
        execution_ids: Vec<ExecutionId>,
    },
    RequestExecutionRelease {
        execution_ids: Vec<ExecutionId>,
    },
}

// Empty struct variants, unlike internally tagged unit variants, reject unknown
// fields in serde. This keeps forged authority fields out of the command wire.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum TrainingCommandWire {
    Request {},
    Stop {},
    Resume {},
    AuthorizeController {},
    ResolveRequest {
        request_ref: Uuid,
        request_revision: u64,
        approve: bool,
    },
    RequestCandidateDecision {
        selection: Vec<TrainingCandidateSelection>,
    },
    ReleaseHeld {
        review_refs: Vec<Uuid>,
    },
    RequestRelease {
        review_refs: Vec<Uuid>,
    },
    ResolveRelease {
        request_ref: Uuid,
        request_revision: u64,
        approve: bool,
    },
    ReleaseExecutions {
        execution_ids: Vec<ExecutionId>,
    },
    RequestExecutionRelease {
        execution_ids: Vec<ExecutionId>,
    },
}
impl From<TrainingCommandWire> for TrainingCommand {
    fn from(value: TrainingCommandWire) -> Self {
        match value {
            TrainingCommandWire::Request {} => Self::Request,
            TrainingCommandWire::Stop {} => Self::Stop,
            TrainingCommandWire::Resume {} => Self::Resume,
            TrainingCommandWire::AuthorizeController {} => Self::AuthorizeController,
            TrainingCommandWire::ResolveRequest {
                request_ref,
                request_revision,
                approve,
            } => Self::ResolveRequest {
                request_ref,
                request_revision,
                approve,
            },
            TrainingCommandWire::RequestCandidateDecision { selection } => {
                Self::RequestCandidateDecision { selection }
            }
            TrainingCommandWire::ReleaseHeld { review_refs } => Self::ReleaseHeld { review_refs },
            TrainingCommandWire::RequestRelease { review_refs } => {
                Self::RequestRelease { review_refs }
            }
            TrainingCommandWire::ResolveRelease {
                request_ref,
                request_revision,
                approve,
            } => Self::ResolveRelease {
                request_ref,
                request_revision,
                approve,
            },
            TrainingCommandWire::ReleaseExecutions { execution_ids } => {
                Self::ReleaseExecutions { execution_ids }
            }
            TrainingCommandWire::RequestExecutionRelease { execution_ids } => {
                Self::RequestExecutionRelease { execution_ids }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateDecisionAction {
    Approve,
    Reject { reason: String },
    RequestRevision { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingCandidateSelection {
    pub review_ref: Uuid,
    pub candidate_hash: String,
    pub guidance_digest: String,
    pub action: CandidateDecisionAction,
    #[serde(default)]
    pub outcome_confirmation: HumanOutcomeConfirmation,
}

/// Proposed confirmation is not evidence until a verified human accepts the
/// exact request. It never overwrites the original seal's assurance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "confirmation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HumanOutcomeConfirmation {
    #[default]
    NotProvided,
    Confirmed {
        execution_ids: Vec<ExecutionId>,
        statement: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrainingRequestPurpose {
    Consent,
    CandidateDecision {
        selection: Vec<TrainingCandidateSelection>,
    },
    ReleaseHeld {
        review_refs: Vec<Uuid>,
    },
    ReleaseExecutions {
        execution_ids: Vec<ExecutionId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingRequestState {
    Pending,
    Approved,
    Declined,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingDecisionRequest {
    pub request_ref: Uuid,
    pub scope: LibraryScope,
    pub logical_session: LogicalSessionId,
    pub revision: u64,
    pub requester: String,
    pub purpose: TrainingRequestPurpose,
    pub state: TrainingRequestState,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingCommandEnvelope {
    pub idempotency_key: Uuid,
    pub expected_revision: u64,
    pub command: TrainingCommand,
}

#[derive(Debug, thiserror::Error)]
pub enum TrainingError {
    #[error("training command is invalid")]
    InvalidCommand,
    #[error("training action is denied")]
    Denied,
    #[error("training reference is unavailable")]
    Unavailable,
    #[error("training revision or idempotency conflict")]
    Conflict,
    #[error("training is inactive")]
    Inactive,
    #[error("training contribution is held")]
    Held,
    #[error("training grant is expired or revoked")]
    GrantUnavailable,
    #[error("training persistence: {0}")]
    Database(#[from] sqlx::Error),
    #[error("training serialization: {0}")]
    Serialization(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, TrainingError>;

/// A decision from trusted project policy, not deserializable tool input.
#[derive(Debug, Clone)]
pub struct TrainingAuthorization {
    pub(crate) grant: TrainingGrant,
}
impl TrainingAuthorization {
    pub fn grant(&self) -> &TrainingGrant {
        &self.grant
    }
}

#[async_trait]
pub trait TrainingAuthority: Send + Sync {
    async fn authorize_training(
        &self,
        actor: &TrustedTrainingActor,
        logical_session: LogicalSessionId,
        action: TrainingAction,
    ) -> Result<TrainingAuthorization>;
}

#[derive(Debug, Clone)]
pub struct TrainingEpochPermit {
    epoch_ref: Uuid,
    grant_ref: Uuid,
    publication_policy: PublicationPolicy,
}
impl TrainingEpochPermit {
    pub fn epoch_id(&self) -> Uuid {
        self.epoch_ref
    }
    pub fn grant_id(&self) -> Uuid {
        self.grant_ref
    }
    pub fn publication_policy(&self) -> &PublicationPolicy {
        &self.publication_policy
    }
}

type Transaction<'a> = sqlx_tracing::Transaction<'a, sqlx::Postgres>;

#[path = "training_repository.rs"]
mod persistence;

/// Authority bound to an imported review; delegated grants remain revocable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "authority", rename_all = "snake_case")]
pub(super) enum ImportAttestationPermit {
    HumanOwner,
    Delegated {
        grant_id: Uuid,
        grant_revision: u64,
        expires_at: DateTime<Utc>,
    },
}

pub(super) async fn authorize_import_attestation(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
) -> Result<ImportAttestationPermit> {
    if actor.is_human_owner() {
        return Ok(ImportAttestationPermit::HumanOwner);
    }
    let grant = persistence::find_project_grant(tx, actor, TrainingAction::AttestImportedReview)
        .await?
        .ok_or(TrainingError::Denied)?;
    Ok(ImportAttestationPermit::Delegated {
        grant_id: grant.id,
        grant_revision: grant.revision,
        expires_at: grant.expires_at,
    })
}

pub(super) async fn recheck_import_attestation(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    permit: &ImportAttestationPermit,
) -> Result<()> {
    match permit {
        ImportAttestationPermit::HumanOwner => Ok(()),
        ImportAttestationPermit::Delegated {
            grant_id,
            grant_revision,
            expires_at,
        } => {
            persistence::recheck_project_attestation(
                tx,
                scope,
                *grant_id,
                *grant_revision,
                *expires_at,
            )
            .await
        }
    }
}
pub use persistence::{
    authorize_publication, authorize_review, lock_training_epoch, lock_training_processing,
    lock_training_processing_epoch, lock_training_project, validate_training_epoch,
};

#[path = "training_requests.rs"]
mod requests;
pub use requests::{
    complete_candidate_decision_request, lock_candidate_decision_request,
    validate_human_confirmation,
};

#[path = "training_release.rs"]
mod release;
pub use release::{authorize_review_publication, lock_training_review};

#[path = "training_run_release.rs"]
mod run_release;
pub use run_release::{authorize_review_executions, bind_released_review};

#[cfg(test)]
#[path = "training_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedTrainingGrant {
    pub grant_ref: Uuid,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingIssuerRevocation {
    pub issuer_subject: String,
    /// Each affected grant's committed revision; length is the revoked count.
    pub grants: Vec<RevokedTrainingGrant>,
}
