//! SQLx ledger: immutable receipts/seals, fenced jobs and serialized bounded promotion.
use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::model::*;
use crate::traced_pg::PgPool;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionRunSummary {
    pub run_id: RunArtifactId,
    pub plan_id: CompiledProgramId,
    pub session_revision: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("workflow learning database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("workflow learning serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("workflow learning evidence is incomplete: {0}")]
    InvalidEvidence(&'static str),
    #[error("stored workflow evidence uses an incompatible teaching format; retained for repair")]
    IncompatibleTeachingFormat,
    #[error("immutable workflow evidence conflicts with stored revision")]
    ImmutableConflict,
    #[error("review worker lease expired or was superseded")]
    LeaseConflict,
    #[error("workflow portfolio changed; recompute admission")]
    PortfolioConflict,
    #[error("workflow example is unavailable in this scope")]
    Unavailable,
    #[error("PLASM_WORKFLOW_LEARNING_TENANT_CAP must be a positive integer")]
    InvalidTenantCap,
    #[error("review is unavailable for scoped publication approval")]
    ApprovalUnavailable,
}

fn validate_seal_format(value: &serde_json::Value) -> std::result::Result<(), RepositoryError> {
    let Some(receipts) = value
        .get("executions")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(RepositoryError::IncompatibleTeachingFormat);
    };
    for receipt in receipts {
        if receipt.get("observation").is_none()
            || receipt.get("catalogue_requirements").is_none()
            || (receipt
                .pointer("/observation/state")
                .and_then(serde_json::Value::as_str)
                == Some("available")
                && (receipt
                    .pointer("/observation/value/program/format")
                    .and_then(serde_json::Value::as_str)
                    != Some("whole_program_example")
                    || receipt
                        .pointer("/observation/value/program/schema_version")
                        .and_then(serde_json::Value::as_u64)
                        != Some(u64::from(super::teaching::TEACHING_SCHEMA_VERSION))))
        {
            return Err(RepositoryError::IncompatibleTeachingFormat);
        }
    }
    Ok(())
}

type Result<T> = std::result::Result<T, RepositoryError>;

#[path = "archive_repository.rs"]
mod archive_repository;
use archive_repository::PublicationSupport;

#[path = "repository_frozen.rs"]
mod frozen;
pub use frozen::{FrozenProposalArtifact, FrozenPublicationReceipt, FrozenPublicationRequest};

#[path = "normalization_audit.rs"]
mod normalization_audit;
pub use normalization_audit::{
    NormalizationCapture, NormalizationContext, NormalizationOutcome, NormalizationStage,
    PendingNormalizationCall,
};

#[path = "repository_guidance.rs"]
mod guidance;
pub use guidance::{FrozenReviewArtifacts, GuidanceSearch, ReviewInspectionIncompatibility};

#[derive(Clone, Copy)]
enum ProposalOrigin {
    Worker,
    FrozenReplay,
}

#[derive(Clone)]
pub struct WorkflowRepository {
    pool: PgPool,
    tenant_cap: i64,
}

/// One bounded, profile-compatible recall request inside an authenticated scope.
pub struct ExampleSearch<'a> {
    pub intent: &'a super::index::NormalizedIntent,
    pub embedding: &'a [f32],
    pub profile: &'a str,
    pub catalogs: &'a BTreeSet<String>,
    pub limit: u32,
    pub active_only: bool,
}

/// Keeps the exact active revision stable until the caller's session publication.
/// The traced pool ties this owned transaction's lifetime to its repository borrow.
pub struct ActiveExampleGuard<'a> {
    pub example: StoredExample,
    transaction: sqlx_tracing::Transaction<'a, sqlx::Postgres>,
}

impl ActiveExampleGuard<'_> {
    pub async fn commit(self) -> Result<()> {
        self.transaction.commit().await?;
        Ok(())
    }
}

/// A seal/job remains invisible until the authoritative session gate is finalized.
pub struct PendingReview<'a> {
    pub handle: ReviewHandle,
    transaction: sqlx_tracing::Transaction<'a, sqlx::Postgres>,
}

impl PendingReview<'_> {
    pub async fn commit(self) -> Result<ReviewHandle> {
        self.transaction.commit().await?;
        Ok(self.handle)
    }
}

fn revision(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| RepositoryError::InvalidEvidence("session revision overflow"))
}

fn validate_review(request: &ReviewRequest) -> Result<()> {
    if request.scope.tenant.trim().is_empty()
        || request.scope.project.trim().is_empty()
        || request.intent.trim().is_empty()
        || request.subtasks.is_empty()
        || request.subtasks.iter().any(|s| {
            s.intent.trim().is_empty()
                || s.observed_outcome.trim().is_empty()
                || s.run_ids.is_empty()
        })
    {
        return Err(RepositoryError::InvalidEvidence(
            "scope, intent, outcomes and receipt references are required",
        ));
    }
    Ok(())
}

impl WorkflowRepository {
    pub async fn connect(url: &str) -> Result<Self> {
        let tenant_cap = match std::env::var("PLASM_WORKFLOW_LEARNING_TENANT_CAP") {
            Ok(value) => value
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(RepositoryError::InvalidTenantCap)?,
            Err(std::env::VarError::NotPresent) => 1000,
            Err(_) => return Err(RepositoryError::InvalidTenantCap),
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await?;
        Ok(Self {
            pool: crate::traced_pg::wrap(pool),
            tenant_cap,
        })
    }

    pub async fn migrate(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(709471032)")
            .execute(&mut tx.executor())
            .await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&mut tx.executor())
            .await?;
        sqlx::raw_sql(include_str!("archive_schema.sql"))
            .execute(&mut tx.executor())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn bind_session(
        &self,
        scope: &LibraryScope,
        logical_session: LogicalSessionId,
    ) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        if scope.tenant.is_empty() || scope.project.is_empty() {
            return Err(RepositoryError::InvalidEvidence("empty session scope"));
        }
        let project: String = sqlx::query_scalar("INSERT INTO workflow_learning_sessions(tenant,logical_session,project) VALUES($1,$2,$3) ON CONFLICT(tenant,logical_session) DO UPDATE SET project=workflow_learning_sessions.project RETURNING project")
            .bind(&scope.tenant).bind(logical_session.to_string()).bind(&scope.project).fetch_one(&mut connection).await?;
        if project != scope.project {
            return Err(RepositoryError::ImmutableConflict);
        }
        Ok(())
    }

    pub async fn session_scope(
        &self,
        tenant: &str,
        logical_session: LogicalSessionId,
    ) -> Result<Option<LibraryScope>> {
        let mut connection = self.pool.acquire().await?;
        let project: Option<String> = sqlx::query_scalar(
            "SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND logical_session=$2",
        )
        .bind(tenant)
        .bind(logical_session.to_string())
        .fetch_optional(&mut connection)
        .await?;
        Ok(project.map(|project| LibraryScope {
            tenant: tenant.to_owned(),
            project,
        }))
    }

    pub async fn begin_execution(
        &self,
        scope: &LibraryScope,
        logical_session: LogicalSessionId,
        id: &str,
    ) -> Result<()> {
        if id.is_empty() {
            return Err(RepositoryError::InvalidEvidence(
                "empty execution journal identity",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let bound: Option<String> = sqlx::query_scalar("SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND logical_session=$2 FOR UPDATE")
            .bind(&scope.tenant).bind(logical_session.to_string()).fetch_optional(&mut tx.executor()).await?;
        if bound.as_ref() != Some(&scope.project) {
            return Err(RepositoryError::InvalidEvidence(
                "execution scope is unbound",
            ));
        }
        let state: String = sqlx::query_scalar("INSERT INTO workflow_learning_execution_journal(tenant,project,logical_session,execution_id,state) VALUES($1,$2,$3,$4,'pending') ON CONFLICT(tenant,project,logical_session,execution_id) DO UPDATE SET state=workflow_learning_execution_journal.state RETURNING state")
            .bind(&scope.tenant).bind(&scope.project).bind(logical_session.to_string()).bind(id).fetch_one(&mut tx.executor()).await?;
        if state != "pending" {
            return Err(RepositoryError::ImmutableConflict);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn finish_execution(
        &self,
        scope: &LibraryScope,
        logical_session: LogicalSessionId,
        id: &str,
        successful: bool,
    ) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        let target = if successful { "complete" } else { "unknown" };
        let changed = sqlx::query("UPDATE workflow_learning_execution_journal SET state=$1 WHERE tenant=$2 AND project=$3 AND logical_session=$4 AND execution_id=$5 AND (state='pending' OR state=$1)")
            .bind(target).bind(&scope.tenant).bind(&scope.project).bind(logical_session.to_string()).bind(id).execute(&mut connection).await?.rows_affected();
        if changed != 1 {
            return Err(RepositoryError::ImmutableConflict);
        }
        Ok(())
    }

    pub async fn execution_index(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<Vec<ExecutionRunSummary>> {
        let mut connection = self.pool.acquire().await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar("SELECT jsonb_build_object('run_id',run_id,'session_revision',session_revision,'plan_id',receipt->'plan_id') FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 ORDER BY session_revision,run_id LIMIT 1025")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_all(&mut connection).await?;
        if rows.len() > 1024 {
            return Err(RepositoryError::InvalidEvidence(
                "completed run index exceeds 1024 rows",
            ));
        }
        rows.into_iter()
            .map(|value| serde_json::from_value(value).map_err(RepositoryError::from))
            .collect()
    }

    pub async fn execution_receipt(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
        run: RunArtifactId,
    ) -> Result<ExecutionReceipt> {
        let mut connection = self.pool.acquire().await?;
        let payload: serde_json::Value = sqlx::query_scalar("SELECT receipt FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND run_id=$4 ORDER BY session_revision DESC LIMIT 1")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).fetch_optional(&mut connection).await?.ok_or(RepositoryError::Unavailable)?;
        Ok(serde_json::from_value(payload)?)
    }

    /// Bounded authoritative run index; callers cannot submit receipt bodies.
    pub async fn execution_receipts(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<Vec<ExecutionReceipt>> {
        let mut connection = self.pool.acquire().await?;
        let payloads: Vec<serde_json::Value> = sqlx::query_scalar("SELECT receipt FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 ORDER BY session_revision,run_id LIMIT 1024")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_all(&mut connection).await?;
        payloads
            .into_iter()
            .map(|payload| serde_json::from_value(payload).map_err(RepositoryError::from))
            .collect()
    }

    /// Host-only path: caller must have verified completion and retained the full artifact.
    pub async fn record_execution(&self, receipt: &ExecutionReceipt) -> Result<()> {
        if self
            .session_scope(&receipt.scope.tenant, receipt.logical_session)
            .await?
            .as_ref()
            != Some(&receipt.scope)
        {
            return Err(RepositoryError::InvalidEvidence(
                "execution session is not bound to this project",
            ));
        }
        if receipt.source.trim().is_empty()
            || receipt.catalog_fingerprints.is_empty()
            || !receipt.compiled_plan.is_object()
            || !receipt.run_artifact.is_object()
            || receipt.scope.tenant.is_empty()
            || receipt.scope.project.is_empty()
        {
            return Err(RepositoryError::InvalidEvidence(
                "incomplete successful execution receipt",
            ));
        }
        let payload = serde_json::to_value(receipt)?;
        let mut connection = self.pool.acquire().await?;
        let stored: serde_json::Value = sqlx::query_scalar("INSERT INTO workflow_learning_receipts(tenant,project,logical_session,session_revision,run_id,receipt) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant,project,logical_session,session_revision,run_id) DO UPDATE SET receipt=workflow_learning_receipts.receipt RETURNING receipt")
            .bind(&receipt.scope.tenant).bind(&receipt.scope.project).bind(receipt.logical_session.to_string())
            .bind(revision(receipt.session_revision)?).bind(receipt.run_id.to_wire()).bind(&payload)
            .fetch_one(&mut connection).await?;
        if stored != payload {
            return Err(RepositoryError::ImmutableConflict);
        }
        Ok(())
    }

    pub async fn review(&self, request: &ReviewRequest) -> Result<ReviewHandle> {
        self.prepare_review(request).await?.commit().await
    }

    pub async fn prepare_review(&self, request: &ReviewRequest) -> Result<PendingReview<'_>> {
        validate_review(request)?;
        if self
            .session_scope(&request.scope.tenant, request.logical_session)
            .await?
            .as_ref()
            != Some(&request.scope)
        {
            return Err(RepositoryError::InvalidEvidence(
                "review session is not bound to this project",
            ));
        }
        let mut tx = self.pool.begin().await?;
        // Serialize review sealing with the start of every host execution.
        sqlx::query("SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3 FOR UPDATE")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string()).fetch_one(&mut tx.executor()).await?;
        let unresolved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_execution_journal WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND state<>'complete')")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string()).fetch_one(&mut tx.executor()).await?;
        if unresolved {
            return Err(RepositoryError::InvalidEvidence(
                "session contains pending or unknown effects",
            ));
        }
        let existing = sqlx::query("SELECT id,digest,seal FROM workflow_learning_reviews WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND session_revision=$4")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
            .bind(revision(request.session_revision)?).fetch_optional(&mut tx.executor()).await?;
        if let Some(row) = existing {
            let payload: serde_json::Value = row.try_get("seal")?;
            validate_seal_format(&payload)?;
            let previous: SealedReview = serde_json::from_value(payload)?;
            if previous.request != *request {
                return Err(RepositoryError::ImmutableConflict);
            }
            return Ok(PendingReview {
                handle: ReviewHandle {
                    id: row.try_get("id")?,
                    digest: row.try_get("digest")?,
                },
                transaction: tx,
            });
        }
        let wanted: BTreeSet<_> = request
            .subtasks
            .iter()
            .flat_map(|s| s.run_ids.iter())
            .collect();
        let mut executions: Vec<ExecutionReceipt> = Vec::new();
        for run_id in wanted {
            let receipt: Option<serde_json::Value> = sqlx::query_scalar("SELECT receipt FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND session_revision<=$4 AND run_id=$5 ORDER BY session_revision DESC LIMIT 1")
                .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
                .bind(revision(request.session_revision)?).bind(run_id.to_wire()).fetch_optional(&mut tx.executor()).await?;
            let receipt = receipt.ok_or(RepositoryError::InvalidEvidence(
                "run not recorded as successfully completed in this scope/session",
            ))?;
            executions.push(serde_json::from_value(receipt)?);
        }
        executions.sort_by(|left, right| {
            left.session_revision
                .cmp(&right.session_revision)
                .then(left.run_id.cmp(&right.run_id))
        });
        let seal = SealedReview {
            request: request.clone(),
            executions,
            assurance: OutcomeAssurance::AgentAttested,
        };
        let payload = serde_json::to_value(&seal)?;
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&payload)?));
        let row = sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant,project,logical_session,session_revision) DO UPDATE SET digest=workflow_learning_reviews.digest RETURNING id,digest")
            .bind(Uuid::new_v4()).bind(&request.scope.tenant).bind(&request.scope.project)
            .bind(request.logical_session.to_string()).bind(revision(request.session_revision)?).bind(&digest)
            .bind(payload).fetch_one(&mut tx.executor()).await?;
        if row.try_get::<String, _>("digest")? != digest {
            return Err(RepositoryError::ImmutableConflict);
        }
        let handle = ReviewHandle {
            id: row.try_get("id")?,
            digest,
        };
        Ok(PendingReview {
            handle,
            transaction: tx,
        })
    }

    pub async fn claim(&self, lease_seconds: u32) -> Result<Option<ClaimedReview>> {
        self.claim_scoped(lease_seconds, None).await
    }

    pub(super) async fn claim_scoped(
        &self,
        lease_seconds: u32,
        scope: Option<&LibraryScope>,
    ) -> Result<Option<ClaimedReview>> {
        self.claim_available(lease_seconds, scope, false).await
    }

    pub(super) async fn claim_recorded_scoped(
        &self,
        lease_seconds: u32,
        scope: Option<&LibraryScope>,
    ) -> Result<Option<ClaimedReview>> {
        self.claim_available(lease_seconds, scope, true).await
    }

    async fn claim_available(
        &self,
        lease_seconds: u32,
        scope: Option<&LibraryScope>,
        recorded_only: bool,
    ) -> Result<Option<ClaimedReview>> {
        let mut connection = self.pool.acquire().await?;
        if lease_seconds == 0 {
            return Err(RepositoryError::InvalidEvidence("lease must be positive"));
        }
        // Expired workers consume the same retry budget as reported failures.
        // SKIP LOCKED avoids waiting on a publisher committing its transaction.
        sqlx::query("WITH expired AS (SELECT id FROM workflow_learning_reviews WHERE state='leased' AND lease_until<=clock_timestamp() AND ($1::text IS NULL OR (tenant=$1 AND project=$2)) AND (NOT $3 OR EXISTS(SELECT 1 FROM workflow_learning_proposal_batches b WHERE b.review_id=workflow_learning_reviews.id)) FOR UPDATE SKIP LOCKED) UPDATE workflow_learning_reviews r SET state=CASE WHEN r.attempts+1<3 THEN 'queued' ELSE 'failed' END,attempts=r.attempts+1,next_attempt=clock_timestamp()+make_interval(secs=>15.0*power(2.0,least(r.attempts,2))),failure_reason='worker lease expired' FROM expired WHERE r.id=expired.id")
            .bind(scope.map(|s|s.tenant.as_str())).bind(scope.map(|s|s.project.as_str())).bind(recorded_only).execute(&mut connection).await?;
        let row = sqlx::query("WITH next AS (SELECT id FROM workflow_learning_reviews WHERE attempts<3 AND next_attempt<=clock_timestamp() AND state='queued' AND ($2::text IS NULL OR (tenant=$2 AND project=$3)) AND (NOT $4 OR EXISTS(SELECT 1 FROM workflow_learning_proposal_batches b WHERE b.review_id=workflow_learning_reviews.id)) ORDER BY next_attempt,created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE workflow_learning_reviews r SET state='leased',fence=r.fence+1,lease_until=clock_timestamp()+make_interval(secs=>$1::double precision) FROM next WHERE r.id=next.id RETURNING r.id,r.fence,r.seal")
            .bind(f64::from(lease_seconds)).bind(scope.map(|s|s.tenant.as_str())).bind(scope.map(|s|s.project.as_str())).bind(recorded_only).fetch_optional(&mut connection).await?;
        row.map(|row| {
            Ok(ClaimedReview {
                id: row.try_get("id")?,
                fence: row.try_get("fence")?,
                seal: serde_json::from_value(row.try_get("seal")?)?,
            })
        })
        .transpose()
    }

    pub(super) async fn promotion_counts(
        &self,
        scope: &LibraryScope,
    ) -> Result<super::native_admin::PromotionCounts> {
        let mut connection = self.pool.acquire().await?;
        let row = sqlx::query("SELECT count(*) FILTER(WHERE state='queued') AS queued,count(*) FILTER(WHERE state='leased') AS leased,count(*) FILTER(WHERE state='completed') AS completed,count(*) FILTER(WHERE state='failed') AS failed,count(*) FILTER(WHERE state='awaiting_approval') AS awaiting_approval FROM workflow_learning_reviews WHERE tenant=$1 AND project=$2")
            .bind(&scope.tenant).bind(&scope.project).fetch_one(&mut connection).await?;
        let count = |name| -> Result<u64> {
            u64::try_from(row.try_get::<i64, _>(name)?)
                .map_err(|_| RepositoryError::InvalidEvidence("negative promotion count"))
        };
        Ok(super::native_admin::PromotionCounts {
            queued: count("queued")?,
            leased: count("leased")?,
            completed: count("completed")?,
            failed: count("failed")?,
            awaiting_approval: count("awaiting_approval")?,
        })
    }

    pub async fn job_failure(
        &self,
        claim: &ClaimedReview,
        reason: &str,
        retryable: bool,
    ) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        let changed = sqlx::query("UPDATE workflow_learning_reviews SET state=CASE WHEN $1 AND attempts+1<3 THEN 'queued' ELSE 'failed' END,attempts=attempts+1,next_attempt=clock_timestamp()+make_interval(secs=>15.0*power(2.0,least(attempts,2))),failure_reason=$2 WHERE id=$3 AND fence=$4 AND state='leased' AND lease_until>clock_timestamp()")
            .bind(retryable).bind(reason).bind(claim.id).bind(claim.fence)
            .execute(&mut connection).await?.rows_affected();
        if changed != 1 {
            return Err(RepositoryError::LeaseConflict);
        }
        Ok(())
    }

    /// Trusted operator API. Approval is scoped, immutable, audited, and causes
    /// revalidation of the original seal; it never publishes directly.
    pub async fn approve_publication(
        &self,
        scope: &LibraryScope,
        review_id: Uuid,
        actor: &str,
    ) -> Result<()> {
        if actor.trim().is_empty() {
            return Err(RepositoryError::InvalidEvidence(
                "approval actor is required",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT state,seal FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 FOR UPDATE")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?
            .ok_or(RepositoryError::ApprovalUnavailable)?;
        validate_seal_format(&row.try_get::<serde_json::Value, _>("seal")?)?;
        let state: String = row.try_get("state")?;
        let approved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_publication_approvals WHERE review_id=$1 AND tenant=$2 AND project=$3)")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
        if approved {
            return Ok(());
        }
        if state != "awaiting_approval" {
            return Err(RepositoryError::ApprovalUnavailable);
        }
        sqlx::query("INSERT INTO workflow_learning_publication_approvals(review_id,tenant,project,actor) VALUES($1,$2,$3,$4)")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).bind(actor).execute(&mut tx.executor()).await?;
        sqlx::query("UPDATE workflow_learning_reviews SET state='queued',next_attempt=clock_timestamp() WHERE id=$1")
            .bind(review_id).execute(&mut tx.executor()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn portfolio_version(
        &self,
        scope: &LibraryScope,
        family: &str,
        variant: &str,
    ) -> Result<i64> {
        let mut connection = self.pool.acquire().await?;
        Ok(sqlx::query_scalar("SELECT version FROM workflow_learning_families WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
            .bind(scope.tenant.clone()).bind(scope.project.clone()).bind(family.to_owned()).bind(variant.to_owned())
            .fetch_optional(&mut connection).await?.unwrap_or(0))
    }

    /// Only trusted worker output enters this API, never unvalidated MCP JSON.
    pub async fn finish(
        &self,
        claim: &ClaimedReview,
        candidates: &[Candidate],
        policy: PublicationPolicy,
    ) -> Result<()> {
        self.finish_in_transaction(
            self.pool.begin().await?,
            claim,
            candidates,
            policy,
            ProposalOrigin::Worker,
        )
        .await
    }

    async fn finish_in_transaction(
        &self,
        mut tx: sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        claim: &ClaimedReview,
        candidates: &[Candidate],
        policy: PublicationPolicy,
        origin: ProposalOrigin,
    ) -> Result<()> {
        if candidates.len() > 32 {
            return Err(RepositoryError::InvalidEvidence(
                "review candidate batch exceeds 32",
            ));
        }
        let seal: Option<serde_json::Value> = sqlx::query_scalar("SELECT seal FROM workflow_learning_reviews WHERE id=$1 AND fence=$2 AND state='leased' AND lease_until>clock_timestamp() FOR UPDATE")
            .bind(claim.id).bind(claim.fence).fetch_optional(&mut tx.executor()).await?;
        let seal: SealedReview =
            serde_json::from_value(seal.ok_or(RepositoryError::LeaseConflict)?)?;
        let scope = &seal.request.scope;
        if matches!(origin, ProposalOrigin::Worker) {
            frozen::record_proposals(&mut tx, claim.id, candidates).await?;
        }
        let approved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_publication_approvals WHERE review_id=$1 AND tenant=$2 AND project=$3)")
            .bind(claim.id).bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
        let publish = policy.permits_publication(approved);
        sqlx::query(
            "INSERT INTO workflow_learning_tenants(tenant) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(&scope.tenant)
        .execute(&mut tx.executor())
        .await?;
        sqlx::query("SELECT tenant FROM workflow_learning_tenants WHERE tenant=$1 FOR UPDATE")
            .bind(&scope.tenant)
            .fetch_one(&mut tx.executor())
            .await?;
        self.publish_guidance_in_transaction(&mut tx, claim.id, &seal, publish)
            .await?;
        self.publish_candidates_in_transaction(
            &mut tx,
            scope,
            &seal,
            candidates,
            policy,
            approved,
            &PublicationSupport::LocalReview(claim.id),
        )
        .await?;
        // Recheck real time after locks and all writes; expiry rolls back the whole publication.
        let finished_state = if policy == PublicationPolicy::Reviewed && !approved {
            "awaiting_approval"
        } else {
            "completed"
        };
        let changed = sqlx::query("UPDATE workflow_learning_reviews SET state=$3 WHERE id=$1 AND fence=$2 AND lease_until>clock_timestamp()")
            .bind(claim.id).bind(claim.fence).bind(finished_state).execute(&mut tx.executor()).await?.rows_affected();
        if changed != 1 {
            return Err(RepositoryError::LeaseConflict);
        }
        tx.commit().await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn publish_candidates_in_transaction(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        scope: &LibraryScope,
        seal: &SealedReview,
        candidates: &[Candidate],
        policy: PublicationPolicy,
        approved: bool,
        support: &PublicationSupport,
    ) -> Result<()> {
        let publish = policy.permits_publication(approved);
        let mut ordered: Vec<_> = candidates.iter().collect();
        ordered.sort_by(|left, right| {
            (
                &left.family,
                &left.variant,
                left.coverage.iter().collect::<BTreeSet<_>>(),
                &left.identity,
            )
                .cmp(&(
                    &right.family,
                    &right.variant,
                    right.coverage.iter().collect::<BTreeSet<_>>(),
                    &right.identity,
                ))
        });
        let mut families = BTreeMap::new();
        for candidate in ordered {
            if candidate.embedding_profile.trim().is_empty() {
                return Err(RepositoryError::InvalidEvidence(
                    "embedding profile is required",
                ));
            }
            if candidate.embedding.iter().any(|value| !value.is_finite()) {
                return Err(RepositoryError::InvalidEvidence(
                    "non-finite candidate embedding",
                ));
            }
            let family_key = (candidate.family.clone(), candidate.variant.clone());
            if let Some(expected) = families.get(&family_key) {
                if matches!(support, PublicationSupport::LocalReview(_))
                    && *expected != candidate.expected_family_version
                {
                    return Err(RepositoryError::PortfolioConflict);
                }
            } else {
                sqlx::query("INSERT INTO workflow_learning_families(tenant,project,family,variant) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).execute(&mut tx.executor()).await?;
                let version: i64 = sqlx::query_scalar("SELECT version FROM workflow_learning_families WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4 FOR UPDATE")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).fetch_one(&mut tx.executor()).await?;
                if matches!(support, PublicationSupport::LocalReview(_))
                    && version != candidate.expected_family_version
                {
                    return Err(RepositoryError::PortfolioConflict);
                }
                families.insert(family_key, version);
            }
            let rows = sqlx::query("SELECT id,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4 AND state='active' ORDER BY created_at,id")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).fetch_all(&mut tx.executor()).await?;
            let mut active = Vec::new();
            for row in rows {
                active.push((
                    row.try_get::<Uuid, _>("id")?,
                    serde_json::from_value::<Candidate>(row.try_get("candidate")?)?,
                ));
            }
            let duplicate = sqlx::query("SELECT id,state,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4 AND identity=$5 AND NOT (state='bad' AND reason IS NOT DISTINCT FROM 'incompatible teaching format; retained for repair') ORDER BY created_at,id LIMIT 1")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).bind(&candidate.identity)
                .fetch_optional(&mut tx.executor()).await?;
            let existing = duplicate
                .map(|row| -> Result<(Uuid, String, Candidate)> {
                    Ok((
                        row.try_get("id")?,
                        row.try_get("state")?,
                        serde_json::from_value(row.try_get("candidate")?)?,
                    ))
                })
                .transpose()?;
            let can_activate = match &existing {
                None => true,
                Some((id, state, old))
                    if state == "candidate"
                        && old.checked_program() == candidate.checked_program()
                        && old.behavior_fingerprint() == candidate.behavior_fingerprint()
                        && old.catalog_fingerprints == candidate.catalog_fingerprints =>
                {
                    (policy == PublicationPolicy::Reviewed && approved)
                        || !support.has_example(tx, *id).await?
                }
                Some(_) => false,
            };
            let mut state = "candidate";
            let mut victim: Option<(Uuid, &Quality)> = None;
            if can_activate && publish && candidate.verified_by(seal) {
                let coverage: BTreeSet<_> = candidate.coverage.iter().collect();
                for (id, old) in &active {
                    if old.coverage.iter().collect::<BTreeSet<_>>() == coverage
                        && old.behavior_fingerprint() == candidate.behavior_fingerprint()
                        && candidate.quality.strictly_better_than(&old.quality)
                        && victim
                            .as_ref()
                            .is_none_or(|(_, quality)| quality.strictly_better_than(&old.quality))
                    {
                        victim = Some((*id, &old.quality));
                    }
                }
                let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND state='active'")
                    .bind(&scope.tenant).fetch_one(&mut tx.executor()).await?;
                if let Some((id, _)) = victim {
                    sqlx::query(
                        "UPDATE workflow_learning_examples SET state='superseded' WHERE id=$1",
                    )
                    .bind(id)
                    .execute(&mut tx.executor())
                    .await?;
                    state = "active";
                } else if active.len() < 3
                    && count < self.tenant_cap
                    && !active
                        .iter()
                        .any(|(_, old)| old.coverage.iter().collect::<BTreeSet<_>>() == coverage)
                {
                    state = "active";
                }
            }
            let id = if let Some((id, _, _)) = existing {
                if state == "active" {
                    sqlx::query("UPDATE workflow_learning_examples SET state='active',candidate=$1,reason=NULL WHERE id=$2 AND state='candidate'")
                        .bind(serde_json::to_value(candidate)?).bind(id).execute(&mut tx.executor()).await?;
                }
                id
            } else {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                    .bind(id).bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant)
                    .bind(&candidate.identity).bind(serde_json::to_value(candidate)?).bind(state).bind(candidate.diagnostic().map(|d|d.message.as_str())).execute(&mut tx.executor()).await?;
                id
            };
            if candidate.verified_by(seal) {
                support.attach_example(tx, id).await?;
            }
        }
        for (family, variant) in families.keys() {
            sqlx::query("UPDATE workflow_learning_families SET version=version+1 WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(family).bind(variant).execute(&mut tx.executor()).await?;
        }
        Ok(())
    }

    pub async fn list_eligible(
        &self,
        scope: &LibraryScope,
        limit: u32,
    ) -> Result<Vec<StoredExample>> {
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query("SELECT id,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active' ORDER BY created_at,id LIMIT $3")
            .bind(&scope.tenant).bind(&scope.project).bind(i64::from(limit.min(1000))).fetch_all(&mut connection).await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredExample {
                    id: row.try_get("id")?,
                    candidate: serde_json::from_value(row.try_get("candidate")?)?,
                })
            })
            .collect()
    }

    /// Exact hybrid ranking in PostgreSQL, scoped and authorized before scoring.
    /// The result is bounded; JSON-vector cosine is an exact scan, not an ANN index.
    pub async fn search(
        &self,
        scope: &LibraryScope,
        query: ExampleSearch<'_>,
    ) -> Result<Vec<StoredExample>> {
        let ExampleSearch {
            intent,
            embedding,
            profile,
            catalogs,
            limit,
            active_only,
        } = query;
        if embedding.iter().any(|value| !value.is_finite()) {
            return Err(RepositoryError::InvalidEvidence(
                "non-finite query embedding",
            ));
        }
        let lexical = std::iter::once(intent.purpose.as_str())
            .chain(intent.facets.iter().map(String::as_str))
            .chain(intent.aliases.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let vector = serde_json::to_value(embedding)?;
        let allowed: Vec<String> = catalogs.iter().cloned().collect();
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query(r#"
            WITH eligible AS (
                SELECT id,candidate,created_at,
                    to_tsvector('english'::regconfig,
                        coalesce(candidate->'intent'->>'purpose','') || ' ' ||
                        coalesce((candidate->'intent'->'facets')::text,'') || ' ' ||
                        coalesce((candidate->'intent'->'aliases')::text,'')) AS document
                FROM workflow_learning_examples e
                WHERE tenant=$1 AND project=$2
                    AND candidate->>'embedding_profile'=$8
                    AND (($7 AND state='active') OR (NOT $7 AND state IN ('active','candidate','superseded')))
                    AND NOT EXISTS (SELECT 1 FROM jsonb_object_keys(e.candidate->'catalog_fingerprints') k(catalog)
                                    WHERE NOT (k.catalog=ANY($5::text[])))
            ), ranked AS (
                SELECT id,candidate,created_at,
                    ts_rank_cd(document,plainto_tsquery('english'::regconfig,$3))::double precision
                    + coalesce(CASE WHEN jsonb_array_length(candidate->'embedding')=jsonb_array_length($4::jsonb)
                        AND jsonb_array_length($4::jsonb)>0 THEN (
                        SELECT greatest(0.0,least(1.0,
                            sum(a.value::text::double precision*b.value::text::double precision)
                            / nullif(sqrt(sum(power(a.value::text::double precision,2)))
                                     * sqrt(sum(power(b.value::text::double precision,2))),0)))
                        FROM jsonb_array_elements(candidate->'embedding') WITH ORDINALITY a(value,n)
                        JOIN jsonb_array_elements($4::jsonb) WITH ORDINALITY b(value,n) USING(n)
                    ) END,0.0) AS score
                FROM eligible
            ) SELECT id,candidate FROM ranked WHERE score>0
              ORDER BY score DESC,created_at,id LIMIT $6
        "#).bind(scope.tenant.clone()).bind(scope.project.clone()).bind(lexical).bind(vector)
            .bind(allowed).bind(i64::from(limit.min(32))).bind(active_only).bind(profile.to_owned())
            .fetch_all(&mut connection).await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredExample {
                    id: row.try_get("id")?,
                    candidate: serde_json::from_value(row.try_get("candidate")?)?,
                })
            })
            .collect()
    }

    pub async fn retrieve(&self, scope: &LibraryScope, id: Uuid) -> Result<StoredExample> {
        let mut connection = self.pool.acquire().await?;
        let payload: Option<serde_json::Value> = sqlx::query_scalar("SELECT candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND id=$3 AND state='active'")
            .bind(&scope.tenant).bind(&scope.project).bind(id).fetch_optional(&mut connection).await?;
        Ok(StoredExample {
            id,
            candidate: serde_json::from_value(payload.ok_or(RepositoryError::Unavailable)?)?,
        })
    }

    pub async fn lock_active_example(
        &self,
        scope: &LibraryScope,
        id: Uuid,
    ) -> Result<ActiveExampleGuard<'_>> {
        let mut transaction = self.pool.begin().await?;
        let payload: Option<serde_json::Value> = sqlx::query_scalar("SELECT candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND id=$3 AND state='active' FOR SHARE")
            .bind(&scope.tenant).bind(&scope.project).bind(id).fetch_optional(&mut transaction.executor()).await?;
        let example = StoredExample {
            id,
            candidate: serde_json::from_value(payload.ok_or(RepositoryError::Unavailable)?)?,
        };
        Ok(ActiveExampleGuard {
            example,
            transaction,
        })
    }

    pub async fn quarantine(&self, scope: &LibraryScope, id: Uuid, reason: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT tenant FROM workflow_learning_tenants WHERE tenant=$1 FOR UPDATE")
            .bind(&scope.tenant)
            .fetch_optional(&mut tx.executor())
            .await?;
        let row = sqlx::query("UPDATE workflow_learning_examples SET state='bad',reason=$1 WHERE tenant=$2 AND project=$3 AND id=$4 AND state='active' RETURNING family,variant")
            .bind(reason).bind(&scope.tenant).bind(&scope.project).bind(id).fetch_optional(&mut tx.executor()).await?;
        if let Some(row) = row {
            sqlx::query("UPDATE workflow_learning_families SET version=version+1 WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(row.try_get::<String,_>("family")?).bind(row.try_get::<String,_>("variant")?).execute(&mut tx.executor()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "repository_tests.rs"]
mod tests;
