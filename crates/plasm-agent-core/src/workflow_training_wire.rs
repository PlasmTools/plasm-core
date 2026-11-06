//! Transport-neutral request projections. Human resolution is deliberately absent
//! from the model-callable session command type.

use crate::session_identity::LogicalSessionId;
use crate::workflow_learning::{
    model::LibraryScope,
    repository::{TrainingCandidatePage, TrainingCandidateRef, TrainingCandidateSummary},
    training::TrainingCandidateSelection,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workflow_learning::training::{
    TrainingCommand, TrainingCommandEnvelope, TrainingError, TrainingState, TrainingStatus,
};

/// Explicit prospective opt-in at session creation. Omission means ordinary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum CreationTrainingIntent {
    Request { idempotency_key: Uuid },
}

pub(crate) async fn request_on_creation(
    host: &crate::server_state::PlasmHostState,
    actor: &crate::workflow_learning::training::TrustedTrainingActor,
    logical: LogicalSessionId,
    intent: &CreationTrainingIntent,
) -> serde_json::Value {
    let result = async {
        let tenant = actor
            .scope()
            .tenant
            .strip_prefix("tenant:")
            .ok_or(TrainingError::Denied)?;
        if !host.logical_sessions.verify_tenant(logical, tenant).await {
            return Err(TrainingError::Denied);
        }
        let repository = host
            .workflow_learning
            .repository()
            .await
            .map_err(|_| TrainingError::Unavailable)?;
        repository
            .bind_session(actor.scope(), logical)
            .await
            .map_err(|_| TrainingError::Unavailable)?;
        repository.bind_training_controller(actor, logical).await?;
        let CreationTrainingIntent::Request { idempotency_key } = intent;
        repository
            .training_command(
                actor,
                logical,
                &TrainingCommandEnvelope {
                    idempotency_key: *idempotency_key,
                    expected_revision: 0,
                    command: TrainingCommand::Request,
                },
            )
            .await
    }
    .await;
    match result {
        Ok(status) => {
            serde_json::to_value(TrainingStatusResponse::from(status)).expect("training status")
        }
        Err(error) => {
            serde_json::json!({"capture_availability":"unavailable", "error":training_failure(&error).1})
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SessionCommand {
    Request {
        expected_revision: u64,
        idempotency_key: Uuid,
    },
    Stop {
        expected_revision: u64,
        idempotency_key: Uuid,
    },
    Resume {
        expected_revision: u64,
        idempotency_key: Uuid,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CandidateCommand {
    List {
        #[serde(default = "default_candidate_limit")]
        limit: u32,
        #[serde(default)]
        cursor: Option<String>,
    },
    Read {
        candidate_ref: String,
    },
    RequestDecision {
        expected_revision: u64,
        idempotency_key: Uuid,
        selection: Vec<TrainingCandidateSelection>,
    },
    RequestStatus {
        request_ref: Uuid,
    },
    RequestExecutionRelease {
        expected_revision: u64,
        idempotency_key: Uuid,
        execution_ids: Vec<crate::workflow_learning::model::ExecutionId>,
    },
    RequestRelease {
        expected_revision: u64,
        idempotency_key: Uuid,
        review_refs: Vec<Uuid>,
    },
}

pub(crate) fn default_candidate_limit() -> u32 {
    16
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateCursor {
    scope: LibraryScope,
    logical_session: LogicalSessionId,
    after: TrainingCandidateRef,
}

pub(crate) fn candidate_ref(reference: &TrainingCandidateRef) -> String {
    format!("{}.{}", reference.review_ref, reference.candidate_hash)
}

pub(crate) fn parse_candidate_ref(reference: &str) -> Result<TrainingCandidateRef, TrainingError> {
    let (review, hash) = reference
        .split_once('.')
        .ok_or(TrainingError::InvalidCommand)?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(TrainingError::InvalidCommand);
    }
    Ok(TrainingCandidateRef {
        review_ref: Uuid::parse_str(review).map_err(|_| TrainingError::InvalidCommand)?,
        candidate_hash: hash.to_owned(),
    })
}

pub(crate) fn decode_candidate_cursor(
    cursor: Option<&str>,
    scope: &LibraryScope,
    logical_session: LogicalSessionId,
) -> Result<Option<TrainingCandidateRef>, TrainingError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > 4096 {
        return Err(TrainingError::InvalidCommand);
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| TrainingError::InvalidCommand)?;
    let cursor: CandidateCursor =
        serde_json::from_slice(&bytes).map_err(|_| TrainingError::InvalidCommand)?;
    if cursor.scope != *scope || cursor.logical_session != logical_session {
        return Err(TrainingError::InvalidCommand);
    }
    parse_candidate_ref(&candidate_ref(&cursor.after))?;
    Ok(Some(cursor.after))
}

#[derive(Serialize)]
pub(crate) struct CandidateSummaryResponse {
    candidate_ref: String,
    #[serde(flatten)]
    summary: TrainingCandidateSummary,
}

#[derive(Serialize)]
pub(crate) struct CandidatePageResponse {
    candidates: Vec<CandidateSummaryResponse>,
    next: Option<String>,
}

pub(crate) fn candidate_page(
    page: TrainingCandidatePage,
    scope: &LibraryScope,
    logical_session: LogicalSessionId,
) -> Result<CandidatePageResponse, TrainingError> {
    let next = page
        .next
        .map(|after| {
            serde_json::to_vec(&CandidateCursor {
                scope: scope.clone(),
                logical_session,
                after,
            })
            .map(|bytes| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        })
        .transpose()?;
    Ok(CandidatePageResponse {
        candidates: page
            .candidates
            .into_iter()
            .map(|summary| CandidateSummaryResponse {
                candidate_ref: candidate_ref(&summary.reference),
                summary,
            })
            .collect(),
        next,
    })
}

pub(crate) fn approval_url(request_ref: Uuid) -> Option<String> {
    let base = std::env::var("PLASM_TRAINING_APPROVAL_BASE_URL")
        .ok()
        .or_else(crate::mcp_app::mcp_public_api_origin)?;
    let mut url = url::Url::parse(base.trim()).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_path(&format!(
        "{}/training/requests/{request_ref}",
        url.path().trim_end_matches('/')
    ));
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

impl SessionCommand {
    pub(crate) fn envelope(self) -> TrainingCommandEnvelope {
        let (command, expected_revision, idempotency_key) = match self {
            Self::Request {
                expected_revision,
                idempotency_key,
            } => (TrainingCommand::Request, expected_revision, idempotency_key),
            Self::Stop {
                expected_revision,
                idempotency_key,
            } => (TrainingCommand::Stop, expected_revision, idempotency_key),
            Self::Resume {
                expected_revision,
                idempotency_key,
            } => (TrainingCommand::Resume, expected_revision, idempotency_key),
        };
        TrainingCommandEnvelope {
            command,
            expected_revision,
            idempotency_key,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TrainingStatusResponse {
    logical_session_ref: String,
    #[serde(flatten)]
    status: TrainingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_url: Option<String>,
}

impl From<TrainingStatus> for TrainingStatusResponse {
    fn from(status: TrainingStatus) -> Self {
        let approval_url = match &status.training_state {
            TrainingState::AwaitingAuthorization { request_ref, .. } => approval_url(*request_ref),
            _ => None,
        };
        Self {
            logical_session_ref: crate::mcp_logical_ref::format_logical_session_wire_ref(
                status.logical_session,
            ),
            status,
            approval_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TrainingFailure {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn training_failure(error: &TrainingError) -> (axum::http::StatusCode, TrainingFailure) {
    use axum::http::StatusCode;
    let (status, code) = match error {
        TrainingError::InvalidCommand => (StatusCode::BAD_REQUEST, "invalid_command"),
        TrainingError::Denied => (StatusCode::FORBIDDEN, "action_denied"),
        TrainingError::Unavailable => (StatusCode::NOT_FOUND, "reference_unavailable"),
        TrainingError::Conflict => (StatusCode::CONFLICT, "revision_conflict"),
        TrainingError::Inactive => (StatusCode::CONFLICT, "training_inactive"),
        TrainingError::Held => (StatusCode::CONFLICT, "contribution_held"),
        TrainingError::GrantUnavailable => (StatusCode::FORBIDDEN, "grant_unavailable"),
        TrainingError::Database(_) | TrainingError::Serialization(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "learning_unavailable")
        }
    };
    let message = if status == StatusCode::SERVICE_UNAVAILABLE {
        "Training storage is unavailable; ordinary execution remains available.".to_owned()
    } else {
        error.to_string()
    };
    (status, TrainingFailure { code, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_command_cannot_resolve_consent_or_supply_actor() {
        for value in [
            serde_json::json!({"action":"resolve_request","approve":true,"expected_revision":0,"idempotency_key":Uuid::nil()}),
            serde_json::json!({"action":"request","actor":"human","expected_revision":0,"idempotency_key":Uuid::nil()}),
            serde_json::json!({"action":"request","expected_revision":0}),
        ] {
            assert!(serde_json::from_value::<SessionCommand>(value).is_err());
        }
    }

    #[test]
    fn cursor_is_bound_to_project_and_session() {
        let scope = LibraryScope {
            tenant: "tenant:a".into(),
            project: "p".into(),
        };
        let logical = LogicalSessionId::new_v4();
        let reference = TrainingCandidateRef {
            review_ref: Uuid::new_v4(),
            candidate_hash: "a".repeat(64),
        };
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&CandidateCursor {
                scope: scope.clone(),
                logical_session: logical,
                after: reference,
            })
            .unwrap(),
        );
        assert!(decode_candidate_cursor(Some(&encoded), &scope, logical)
            .unwrap()
            .is_some());
        assert!(
            decode_candidate_cursor(Some(&encoded), &scope, LogicalSessionId::new_v4()).is_err()
        );
        let other = LibraryScope {
            tenant: scope.tenant.clone(),
            project: "other".into(),
        };
        assert!(decode_candidate_cursor(Some(&encoded), &other, logical).is_err());
    }

    #[test]
    fn creation_intent_cannot_claim_consent() {
        assert!(serde_json::from_value::<CreationTrainingIntent>(
            serde_json::json!({"intent":"request","idempotency_key":Uuid::new_v4()})
        )
        .is_ok());
        assert!(serde_json::from_value::<CreationTrainingIntent>(
            serde_json::json!({"intent":"request","idempotency_key":Uuid::new_v4(),"approved":true})
        )
        .is_err());
    }

    #[test]
    fn all_mutations_preserve_revision_and_idempotency() {
        for action in ["request", "stop", "resume"] {
            let id = Uuid::new_v4();
            let command: SessionCommand = serde_json::from_value(
                serde_json::json!({"action":action,"expected_revision":7,"idempotency_key":id}),
            )
            .unwrap();
            let envelope = command.envelope();
            assert_eq!(envelope.expected_revision, 7);
            assert_eq!(envelope.idempotency_key, id);
        }
    }
}
