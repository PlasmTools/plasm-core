//! Scoped durable normalization calls: started before dispatch, terminal evidence
//! written before the worker can consume either an accepted or rejected response.
use super::super::{
    model::{ClaimedReview, LibraryScope},
    normalization::{NormalizationDiagnostic, NormalizationRequest, NormalizedProposal},
};
use super::{RepositoryError, Result, WorkflowRepository};
use plasm_runtime::provider_transport::ProviderAttempt;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

const RAW_LIMIT: usize = 65_536;
const USAGE_LIMIT: usize = 16_384;
const DIAGNOSTIC_LIMIT: usize = 2048;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum NormalizationStage {
    Initial,
    Compare {
        run_id: super::super::model::RunArtifactId,
    },
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
enum Origin {
    Review {
        review_id: Uuid,
        fence: i64,
        logical_session: super::super::model::LogicalSessionId,
        subtask_index: usize,
        stage: NormalizationStage,
    },
    Recall {
        request_id: Uuid,
    },
}
#[derive(Debug, Clone, Serialize)]
pub struct NormalizationContext {
    scope: LibraryScope,
    origin: Origin,
}
impl NormalizationContext {
    pub fn review(
        claim: &ClaimedReview,
        subtask_index: usize,
        stage: NormalizationStage,
    ) -> Result<Self> {
        let subtask = claim.seal.request.subtasks.get(subtask_index).ok_or(
            RepositoryError::InvalidEvidence("normalization subtask is absent from sealed review"),
        )?;
        if let NormalizationStage::Compare { run_id } = &stage {
            if !subtask.run_ids.contains(run_id)
                || !claim
                    .seal
                    .executions
                    .iter()
                    .any(|execution| &execution.run_id == run_id)
            {
                return Err(RepositoryError::InvalidEvidence(
                    "normalization run is not evidence for this subtask",
                ));
            }
        }
        Ok(Self {
            scope: claim.seal.request.scope.clone(),
            origin: Origin::Review {
                review_id: claim.id,
                fence: claim.fence,
                logical_session: claim.seal.request.logical_session,
                subtask_index,
                stage,
            },
        })
    }
    pub fn recall(scope: LibraryScope) -> Self {
        Self {
            scope,
            origin: Origin::Recall {
                request_id: Uuid::new_v4(),
            },
        }
    }
    pub fn scope(&self) -> &LibraryScope {
        &self.scope
    }
}

/// Private context-bound capability minted only after the start record commits.
pub struct PendingNormalizationCall {
    id: Uuid,
    scope: LibraryScope,
}
impl PendingNormalizationCall {
    pub fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundedNormalizationText {
    prefix: String,
    sha256: String,
    bytes: usize,
    truncated: bool,
}
impl BoundedNormalizationText {
    fn capture(raw: &str, limit: usize) -> Self {
        let mut end = raw.len().min(limit);
        while !raw.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            prefix: raw[..end].into(),
            sha256: plasm_core::catalog_discovery::content_hash(raw.as_bytes()),
            bytes: raw.len(),
            truncated: end < raw.len(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UsageEvidence {
    NotReported,
    Reported { raw: BoundedNormalizationText },
    Unavailable { reason: UsageUnavailable },
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum UsageUnavailable {
    MalformedEnvelope,
    ResponseExceedsParsingBudget,
}
#[derive(Debug, Clone, Serialize)]
struct ResponseEvidence {
    http_status: u16,
    raw: BoundedNormalizationText,
    usage: UsageEvidence,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct NormalizationCapture {
    response: Option<ResponseEvidence>,
    transport_diagnostic: Option<BoundedNormalizationText>,
}
impl NormalizationCapture {
    pub fn failure(&mut self, error: &dyn std::error::Error) {
        self.transport_diagnostic = Some(BoundedNormalizationText::capture(
            &error.to_string(),
            DIAGNOSTIC_LIMIT,
        ));
    }

    pub fn record(&mut self, attempt: &ProviderAttempt) {
        match attempt {
            ProviderAttempt::Response {
                http_status,
                raw_response,
                ..
            } => self.response(*http_status, raw_response),
            ProviderAttempt::Transport { error, .. } => {
                self.transport_diagnostic =
                    Some(BoundedNormalizationText::capture(error, DIAGNOSTIC_LIMIT))
            }
            ProviderAttempt::Lifecycle { .. } => {}
        }
    }
    /// The same bounded capture is available to injected offline model transports.
    pub fn response(&mut self, http_status: u16, raw: &str) {
        let usage = if raw.len() > 1_048_576 {
            UsageEvidence::Unavailable {
                reason: UsageUnavailable::ResponseExceedsParsingBudget,
            }
        } else {
            match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(envelope) => envelope
                    .get("usage")
                    .filter(|usage| !usage.is_null())
                    .map(|usage| UsageEvidence::Reported {
                        raw: BoundedNormalizationText::capture(&usage.to_string(), USAGE_LIMIT),
                    })
                    .unwrap_or(UsageEvidence::NotReported),
                Err(_) => UsageEvidence::Unavailable {
                    reason: UsageUnavailable::MalformedEnvelope,
                },
            }
        };
        self.response = Some(ResponseEvidence {
            http_status,
            raw: BoundedNormalizationText::capture(raw, RAW_LIMIT),
            usage,
        });
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NormalizationOutcome {
    Accepted { proposal: NormalizedProposal },
    Rejected { diagnostic: NormalizationDiagnostic },
    TransportFailed,
}
impl NormalizationOutcome {
    fn state(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::TransportFailed => "transport_failed",
        }
    }
}

impl WorkflowRepository {
    pub async fn begin_normalization(
        &self,
        context: NormalizationContext,
        request: &NormalizationRequest,
    ) -> Result<PendingNormalizationCall> {
        if context.scope.tenant.trim().is_empty() || context.scope.project.trim().is_empty() {
            return Err(RepositoryError::InvalidEvidence(
                "normalization scope is empty",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let review_id = if let Origin::Review {
            review_id,
            fence,
            logical_session,
            ..
        } = &context.origin
        {
            let valid:Option<Uuid>=sqlx::query_scalar("SELECT id FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4 AND fence=$5 AND state='leased' AND lease_until>clock_timestamp() FOR SHARE")
                .bind(review_id).bind(&context.scope.tenant).bind(&context.scope.project).bind(logical_session.to_string()).bind(fence).fetch_optional(&mut tx.executor()).await?;
            if valid.is_none() {
                return Err(RepositoryError::LeaseConflict);
            }
            Some(*review_id)
        } else {
            None
        };
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO workflow_learning_normalization_calls(id,tenant,project,review_id,context,model,endpoint,request_hash,request_body,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'pending')")
            .bind(id).bind(&context.scope.tenant).bind(&context.scope.project).bind(review_id).bind(serde_json::to_value(&context)?).bind(request.model()).bind(request.endpoint()).bind(request.request_hash()).bind(request.body()).execute(&mut tx.executor()).await?;
        tx.commit().await?;
        Ok(PendingNormalizationCall {
            id,
            scope: context.scope,
        })
    }

    /// Late completion can preserve evidence after lease expiry; it cannot publish
    /// candidates. Repeating identical completion is idempotent; changes conflict.
    pub async fn finish_normalization(
        &self,
        pending: &PendingNormalizationCall,
        capture: NormalizationCapture,
        outcome: NormalizationOutcome,
    ) -> Result<()> {
        if !matches!(outcome, NormalizationOutcome::TransportFailed) && capture.response.is_none() {
            return Err(RepositoryError::InvalidEvidence(
                "normalization completion is missing provider response evidence",
            ));
        }
        let state = outcome.state();
        let result = serde_json::json!({"capture":capture,"result":outcome});
        if serde_json::to_vec(&result)?.len() > 1_048_576 {
            return Err(RepositoryError::InvalidEvidence(
                "normalization terminal audit exceeds bounded storage",
            ));
        }
        let mut connection = self.pool.acquire().await?;
        let row=sqlx::query("UPDATE workflow_learning_normalization_calls SET state=CASE WHEN state='pending' THEN $4 ELSE state END,result=CASE WHEN state='pending' THEN $5 ELSE result END,finished_at=COALESCE(finished_at,clock_timestamp()) WHERE id=$1 AND tenant=$2 AND project=$3 RETURNING state,result")
            .bind(pending.id).bind(&pending.scope.tenant).bind(&pending.scope.project).bind(state).bind(&result).fetch_optional(&mut connection).await?.ok_or(RepositoryError::Unavailable)?;
        if row.try_get::<String, _>("state")? != state
            || row.try_get::<serde_json::Value, _>("result")? != result
        {
            return Err(RepositoryError::ImmutableConflict);
        }
        Ok(())
    }

    pub async fn normalization_audits(
        &self,
        scope: &LibraryScope,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>> {
        let mut connection = self.pool.acquire().await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(c) FROM workflow_learning_normalization_calls c WHERE tenant=$1 AND project=$2 ORDER BY created_at,id LIMIT $3")
            .bind(&scope.tenant).bind(&scope.project).bind(i64::from(limit.min(1000))).fetch_all(&mut connection).await?)
    }
}

#[cfg(test)]
#[path = "normalization_audit_tests.rs"]
mod tests;
