//! SQLx ledger: immutable receipts/seals, fenced jobs and serialized bounded promotion.
use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::model::*;
use crate::traced_pg::PgPool;

#[path = "repository_recuration.rs"]
mod recuration;
pub use recuration::{
    CrossFamilyExampleView, CrossFamilyRecurationError, CrossFamilyRecurationPlan,
    CrossFamilyRecurationPreview, CrossFamilyRecurationReceipt,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSummary {
    pub execution_id: ExecutionId,
    pub session_revision: u64,
}

pub struct ReviewAuthorization {
    pub actor: training::TrustedTrainingActor,
    pub expected_revision: u64,
    pub idempotency_key: Uuid,
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Training(#[from] training::TrainingError),
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
    serde_json::from_value::<StoredReviewSeal>(value.clone())
        .map(|_| ())
        .map_err(|_| RepositoryError::IncompatibleTeachingFormat)
}

type Result<T> = std::result::Result<T, RepositoryError>;
const SCHEMA_VERSION: i32 = 7;

#[path = "review_storage.rs"]
mod review_storage;
pub(super) use review_storage::hydrate_review;

#[path = "training.rs"]
pub mod training;

#[cfg(test)]
#[path = "repository_test_support.rs"]
mod training_test_support;

#[path = "archive_repository.rs"]
mod archive_repository;
use archive_repository::PublicationSupport;

#[path = "repository_frozen.rs"]
mod frozen;
pub use frozen::{
    FrozenProposalArtifact, FrozenPublicationReceipt, FrozenPublicationRequest,
    PublicationAuthority, TrainingCandidateDetail, TrainingCandidatePage, TrainingCandidateRef,
    TrainingCandidateSummary,
};

#[path = "repository_decisions.rs"]
mod decisions;
pub use decisions::{
    CandidateDecisionOutcome, PublicationDeferral, TrainingCandidateDecisionReceipt,
    TrainingDecisionReceipt,
};

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
    if request.subtasks.len() > 16 || request.subtasks.iter().any(|s| s.execution_ids.len() > 32) {
        return Err(RepositoryError::InvalidEvidence(
            "review execution references exceed limits",
        ));
    }
    if request.scope.tenant.trim().is_empty()
        || request.scope.project.trim().is_empty()
        || request.intent.trim().is_empty()
        || request.subtasks.is_empty()
        || request.subtasks.iter().any(|s| {
            s.intent.trim().is_empty()
                || s.observed_outcome.trim().is_empty()
                || s.execution_ids.is_empty()
        })
    {
        return Err(RepositoryError::InvalidEvidence(
            "scope, intent, outcomes and receipt references are required",
        ));
    }
    Ok(())
}

impl WorkflowRepository {
    /// Rechecked at provider boundaries and publication; a new epoch never
    /// authorizes material from a stopped epoch.
    pub(super) async fn lock_review_training(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        claim: &ClaimedReview,
    ) -> Result<training::TrainingEpochPermit> {
        let epoch: Option<Uuid> = sqlx::query_scalar("SELECT training_epoch FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4")
            .bind(claim.id).bind(&claim.seal.request.scope.tenant).bind(&claim.seal.request.scope.project)
            .bind(claim.seal.request.logical_session.to_string()).fetch_optional(&mut tx.executor()).await?.flatten();
        Ok(training::lock_training_review(
            tx,
            &claim.seal.request.scope,
            claim.seal.request.logical_session,
            claim.id,
            epoch.ok_or(training::TrainingError::Inactive)?,
        )
        .await?)
    }

    pub(super) async fn ensure_review_training(&self, claim: &ClaimedReview) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.lock_review_training(&mut tx, claim).await?;
        tx.commit().await?;
        Ok(())
    }

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
        // One transaction per schema version, including the marker. Reconnecting
        // replicas must not acquire DDL locks against live workflow transactions.
        // Bump this version whenever any embedded schema changes.
        sqlx::query("CREATE TABLE IF NOT EXISTS workflow_learning_schema_versions(version integer PRIMARY KEY,applied_at timestamptz NOT NULL DEFAULT clock_timestamp())")
            .execute(&mut tx.executor()).await?;
        let applied: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workflow_learning_schema_versions WHERE version=$1)",
        )
        .bind(SCHEMA_VERSION)
        .fetch_one(&mut tx.executor())
        .await?;
        if applied {
            let highest: Option<i32> =
                sqlx::query_scalar("SELECT max(version) FROM workflow_learning_schema_versions")
                    .fetch_one(&mut tx.executor())
                    .await?;
            if highest != Some(SCHEMA_VERSION) {
                return Err(RepositoryError::InvalidEvidence(
                    "workflow learning schema is newer than this binary",
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        let prior: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_schema_versions)")
                .fetch_one(&mut tx.executor())
                .await?;
        if prior {
            return Err(RepositoryError::InvalidEvidence(
                "workflow learning store uses an unsupported older schema; create a fresh store",
            ));
        }
        let unversioned: bool =
            sqlx::query_scalar("SELECT to_regclass('workflow_learning_receipts') IS NOT NULL")
                .fetch_one(&mut tx.executor())
                .await?;
        if unversioned {
            return Err(RepositoryError::InvalidEvidence(
                "workflow learning store is unversioned; create a fresh store",
            ));
        }
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&mut tx.executor())
            .await?;
        sqlx::raw_sql(include_str!("archive_schema.sql"))
            .execute(&mut tx.executor())
            .await?;
        sqlx::raw_sql(include_str!("training_schema.sql"))
            .execute(&mut tx.executor())
            .await?;
        sqlx::query(
            "ALTER TABLE workflow_learning_receipts ALTER COLUMN program_digest SET NOT NULL",
        )
        .execute(&mut tx.executor())
        .await?;
        if !prior {
            sqlx::query("ALTER TABLE workflow_learning_receipts ADD CONSTRAINT workflow_learning_receipts_program_fkey FOREIGN KEY(tenant,project,program_digest) REFERENCES workflow_learning_program_observations(tenant,project,digest)")
            .execute(&mut tx.executor()).await?;
        }
        sqlx::query(
            "ALTER TABLE workflow_learning_reviews ALTER COLUMN seal_content_digest SET NOT NULL",
        )
        .execute(&mut tx.executor())
        .await?;
        sqlx::query("INSERT INTO workflow_learning_schema_versions(version) VALUES($1)")
            .bind(SCHEMA_VERSION)
            .execute(&mut tx.executor())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Read-only guard for exporters; never migrates or acquires schema locks.
    pub async fn validate_current_schema(&self) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        let version: Option<i32> =
            sqlx::query_scalar("SELECT max(version) FROM workflow_learning_schema_versions")
                .fetch_one(&mut connection)
                .await?;
        if version != Some(SCHEMA_VERSION) {
            return Err(RepositoryError::InvalidEvidence(
                "workflow learning schema is not current",
            ));
        }
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
    ) -> Result<Option<training::TrainingEpochPermit>> {
        if id.is_empty() {
            return Err(RepositoryError::InvalidEvidence(
                "empty execution journal identity",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let permit = match training::lock_training_epoch(&mut tx, scope, logical_session).await {
            Ok(permit) => permit,
            Err(training::TrainingError::Inactive | training::TrainingError::Held) => {
                return Ok(None)
            }
            Err(error) => return Err(error.into()),
        };
        let bound: Option<String> = sqlx::query_scalar("SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND logical_session=$2 FOR UPDATE")
            .bind(&scope.tenant).bind(logical_session.to_string()).fetch_optional(&mut tx.executor()).await?;
        if bound.as_ref() != Some(&scope.project) {
            return Err(RepositoryError::InvalidEvidence(
                "execution scope is unbound",
            ));
        }
        let state: String = sqlx::query_scalar("INSERT INTO workflow_learning_execution_journal(tenant,project,logical_session,execution_id,state,training_epoch) VALUES($1,$2,$3,$4,'pending',$5) ON CONFLICT(tenant,project,logical_session,execution_id) DO UPDATE SET state=workflow_learning_execution_journal.state WHERE workflow_learning_execution_journal.training_epoch=EXCLUDED.training_epoch RETURNING state")
            .bind(&scope.tenant).bind(&scope.project).bind(logical_session.to_string()).bind(id).bind(permit.epoch_id()).fetch_one(&mut tx.executor()).await?;
        if state != "pending" {
            return Err(RepositoryError::ImmutableConflict);
        }
        tx.commit().await?;
        Ok(Some(permit))
    }

    pub async fn finish_execution(
        &self,
        scope: &LibraryScope,
        logical_session: LogicalSessionId,
        id: &str,
        successful: bool,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let epoch: Option<Uuid> = sqlx::query_scalar("SELECT training_epoch FROM workflow_learning_execution_journal WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND execution_id=$4")
            .bind(&scope.tenant).bind(&scope.project).bind(logical_session.to_string()).bind(id).fetch_optional(&mut tx.executor()).await?.flatten();
        if successful {
            training::validate_training_epoch(
                &mut tx,
                scope,
                logical_session,
                epoch.ok_or(training::TrainingError::Inactive)?,
            )
            .await?;
        }
        let target = if successful { "complete" } else { "unknown" };
        let changed = sqlx::query("UPDATE workflow_learning_execution_journal SET state=$1 WHERE tenant=$2 AND project=$3 AND logical_session=$4 AND execution_id=$5 AND (state='pending' OR state=$1)")
            .bind(target).bind(&scope.tenant).bind(&scope.project).bind(logical_session.to_string()).bind(id).execute(&mut tx.executor()).await?.rows_affected();
        if changed != 1 {
            return Err(RepositoryError::ImmutableConflict);
        }
        tx.commit().await?;
        Ok(())
    }

    /// A successful business operation excluded by a consent boundary has no
    /// training success receipt; it must not poison the next epoch's watermark.
    pub(crate) async fn discard_execution(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
        id: &str,
    ) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("UPDATE workflow_learning_execution_journal SET state='excluded' WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND execution_id=$4 AND state IN ('pending','excluded')")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(id).execute(&mut connection).await?;
        Ok(())
    }

    pub async fn execution_index(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<Vec<ExecutionSummary>> {
        let mut connection = self.pool.acquire().await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar("SELECT jsonb_build_object('execution_id',execution_id,'session_revision',session_revision) FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 ORDER BY session_revision,execution_id LIMIT 1025")
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
        run: ExecutionId,
    ) -> Result<ExecutionReceipt> {
        let mut connection = self.pool.acquire().await?;
        let row = sqlx::query("SELECT r.session_revision,r.execution_id,r.receipt,r.program_digest,p.observation FROM workflow_learning_receipts r JOIN workflow_learning_program_observations p ON p.tenant=r.tenant AND p.project=r.project AND p.digest=r.program_digest WHERE r.tenant=$1 AND r.project=$2 AND r.logical_session=$3 AND r.execution_id=$4 ORDER BY r.session_revision DESC LIMIT 1")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).fetch_optional(&mut connection).await?.ok_or(RepositoryError::Unavailable)?;
        review_storage::hydrate_row(scope, logical, row)
    }

    /// Bounded authoritative run index; callers cannot submit receipt bodies.
    pub async fn execution_receipts(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<Vec<ExecutionReceipt>> {
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query("SELECT r.session_revision,r.execution_id,r.receipt,r.program_digest,p.observation FROM workflow_learning_receipts r JOIN workflow_learning_program_observations p ON p.tenant=r.tenant AND p.project=r.project AND p.digest=r.program_digest WHERE r.tenant=$1 AND r.project=$2 AND r.logical_session=$3 ORDER BY r.session_revision,r.execution_id LIMIT 1024")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_all(&mut connection).await?;
        rows.into_iter()
            .map(|row| review_storage::hydrate_row(scope, logical, row))
            .collect()
    }

    /// Host-only path: caller must have verified completion and retained the full artifact.
    pub async fn record_execution(
        &self,
        receipt: &ExecutionReceipt,
        permit: &training::TrainingEpochPermit,
    ) -> Result<()> {
        self.record_execution_batch(std::slice::from_ref(receipt), permit, None)
            .await
    }

    pub(crate) async fn record_execution_batch(
        &self,
        receipts: &[ExecutionReceipt],
        permit: &training::TrainingEpochPermit,
        journal: Option<&str>,
    ) -> Result<()> {
        let first = receipts.first().ok_or(RepositoryError::InvalidEvidence(
            "missing completed execution receipts",
        ))?;
        let mut tx = self.pool.begin().await?;
        training::validate_training_epoch(
            &mut tx,
            &first.scope,
            first.logical_session,
            permit.epoch_id(),
        )
        .await?;
        for receipt in receipts {
            if receipt.scope != first.scope
                || receipt.logical_session != first.logical_session
                || receipt.source.trim().is_empty()
                || receipt.catalog_fingerprints.is_empty()
                || !receipt.compiled_plan.is_object()
            {
                return Err(RepositoryError::InvalidEvidence(
                    "incomplete or mixed execution receipts",
                ));
            }
            let program_digest = review_storage::store_program(&mut tx, receipt).await?;
            let payload = serde_json::to_value(receipt.stored_fact())?;
            let stored: Option<serde_json::Value>=sqlx::query_scalar("INSERT INTO workflow_learning_receipts(tenant,project,logical_session,session_revision,execution_id,receipt,training_epoch,program_digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant,project,logical_session,session_revision,execution_id) DO UPDATE SET receipt=workflow_learning_receipts.receipt WHERE workflow_learning_receipts.training_epoch=EXCLUDED.training_epoch AND workflow_learning_receipts.program_digest=EXCLUDED.program_digest RETURNING receipt")
                .bind(&receipt.scope.tenant).bind(&receipt.scope.project).bind(receipt.logical_session.to_string()).bind(revision(receipt.session_revision)?).bind(receipt.execution_id.to_wire()).bind(&payload).bind(permit.epoch_id()).bind(&program_digest).fetch_optional(&mut tx.executor()).await?;
            if stored.as_ref() != Some(&payload) {
                return Err(RepositoryError::ImmutableConflict);
            }
        }
        if let Some(journal) = journal {
            let changed=sqlx::query("UPDATE workflow_learning_execution_journal SET state='complete' WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND execution_id=$4 AND training_epoch=$5 AND state IN ('pending','complete')")
                .bind(&first.scope.tenant).bind(&first.scope.project).bind(first.logical_session.to_string()).bind(journal).bind(permit.epoch_id()).execute(&mut tx.executor()).await?.rows_affected();
            if changed != 1 {
                return Err(RepositoryError::ImmutableConflict);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn review(
        &self,
        request: &ReviewRequest,
        authorization: &ReviewAuthorization,
    ) -> Result<ReviewHandle> {
        self.prepare_review(request, authorization)
            .await?
            .commit()
            .await
    }

    pub async fn prepare_review(
        &self,
        request: &ReviewRequest,
        authorization: &ReviewAuthorization,
    ) -> Result<PendingReview<'_>> {
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
        let runs = request
            .subtasks
            .iter()
            .flat_map(|subtask| subtask.execution_ids.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let permit = training::authorize_review_executions(
            &mut tx,
            &request.scope,
            request.logical_session,
            &authorization.actor,
            authorization.expected_revision,
            &runs,
        )
        .await?;
        let command = serde_json::to_value(request)?;
        let previous = sqlx::query("SELECT command,review_id,digest FROM workflow_training_review_commands WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND actor=$4 AND credential=$5 AND idempotency_key=$6")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
            .bind(authorization.actor.subject()).bind(authorization.actor.credential()).bind(authorization.idempotency_key)
            .fetch_optional(&mut tx.executor()).await?;
        if let Some(previous) = previous {
            if previous.try_get::<serde_json::Value, _>("command")? != command {
                return Err(RepositoryError::ImmutableConflict);
            }
            return Ok(PendingReview {
                handle: ReviewHandle {
                    id: previous.try_get("review_id")?,
                    digest: previous.try_get("digest")?,
                },
                transaction: tx,
            });
        }
        // Serialize review sealing with the start of every host execution.
        sqlx::query("SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3 FOR UPDATE")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string()).fetch_one(&mut tx.executor()).await?;
        let unresolved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_execution_journal WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND training_epoch=$4 AND state IN ('pending','unknown'))")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string()).bind(permit.epoch_id()).fetch_one(&mut tx.executor()).await?;
        if unresolved {
            return Err(RepositoryError::InvalidEvidence(
                "session contains pending or unknown effects",
            ));
        }
        let existing = sqlx::query("SELECT id,digest,seal,seal_content_digest FROM workflow_learning_reviews WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND session_revision=$4")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
            .bind(revision(request.session_revision)?).fetch_optional(&mut tx.executor()).await?;
        if let Some(row) = existing {
            let payload: serde_json::Value = row.try_get("seal")?;
            let seal_digest: String = row.try_get("seal_content_digest")?;
            if plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&payload)?)
                != seal_digest
            {
                return Err(RepositoryError::ImmutableConflict);
            }
            validate_seal_format(&payload)?;
            let previous: StoredReviewSeal = serde_json::from_value(payload)?;
            if previous.request != *request {
                return Err(RepositoryError::ImmutableConflict);
            }
            sqlx::query("INSERT INTO workflow_training_review_commands(tenant,project,logical_session,actor,credential,idempotency_key,command,review_id,digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
                .bind(authorization.actor.subject()).bind(authorization.actor.credential()).bind(authorization.idempotency_key)
                .bind(&command).bind(row.try_get::<Uuid,_>("id")?).bind(row.try_get::<String,_>("digest")?).execute(&mut tx.executor()).await?;
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
            .flat_map(|s| s.execution_ids.iter())
            .collect();
        let mut executions: Vec<ExecutionReceipt> = Vec::new();
        for execution_id in wanted {
            let row = sqlx::query("SELECT r.session_revision,r.execution_id,r.receipt,r.program_digest,p.observation FROM workflow_learning_receipts r JOIN workflow_learning_program_observations p ON p.tenant=r.tenant AND p.project=r.project AND p.digest=r.program_digest WHERE r.tenant=$1 AND r.project=$2 AND r.logical_session=$3 AND r.session_revision<=$4 AND r.execution_id=$5 AND r.training_epoch=$6 ORDER BY r.session_revision DESC LIMIT 1")
                .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
                .bind(revision(request.session_revision)?).bind(execution_id.to_wire()).bind(permit.epoch_id()).fetch_optional(&mut tx.executor()).await?;
            let row = row.ok_or(RepositoryError::InvalidEvidence(
                "run not recorded as successfully completed in this scope/session",
            ))?;
            executions.push(review_storage::hydrate_row(
                &request.scope,
                request.logical_session,
                row,
            )?);
        }
        executions.sort_by(|left, right| {
            left.session_revision
                .cmp(&right.session_revision)
                .then(left.execution_id.cmp(&right.execution_id))
        });
        let seal = SealedReview {
            request: request.clone(),
            executions,
            assurance: OutcomeAssurance::AgentAttested,
        };
        let payload = serde_json::to_value(StoredReviewSeal::from_review(&seal)?)?;
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&payload)?));
        let row = sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal,training_epoch,seal_content_digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$6) ON CONFLICT(tenant,project,logical_session,session_revision) DO UPDATE SET digest=workflow_learning_reviews.digest RETURNING id,digest")
            .bind(Uuid::new_v4()).bind(&request.scope.tenant).bind(&request.scope.project)
            .bind(request.logical_session.to_string()).bind(revision(request.session_revision)?).bind(&digest)
            .bind(payload).bind(permit.epoch_id()).fetch_one(&mut tx.executor()).await?;
        if row.try_get::<String, _>("digest")? != digest {
            return Err(RepositoryError::ImmutableConflict);
        }
        let handle = ReviewHandle {
            id: row.try_get("id")?,
            digest,
        };
        training::bind_released_review(
            &mut tx,
            &request.scope,
            request.logical_session,
            handle.id,
            &permit,
        )
        .await?;
        sqlx::query("INSERT INTO workflow_training_review_commands(tenant,project,logical_session,actor,credential,idempotency_key,command,review_id,digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(&request.scope.tenant).bind(&request.scope.project).bind(request.logical_session.to_string())
            .bind(authorization.actor.subject()).bind(authorization.actor.credential()).bind(authorization.idempotency_key)
            .bind(command).bind(handle.id).bind(&handle.digest).execute(&mut tx.executor()).await?;
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
        let row = sqlx::query("WITH next AS (SELECT id FROM workflow_learning_reviews r WHERE attempts<3 AND next_attempt<=clock_timestamp() AND state='queued' AND ($2::text IS NULL OR (tenant=$2 AND project=$3)) AND (EXISTS(SELECT 1 FROM workflow_training_epochs e JOIN workflow_training_grants g ON g.id=e.grant_id WHERE e.id=r.training_epoch AND e.tenant=r.tenant AND e.project=r.project AND NOT e.held AND NOT g.revoked AND g.revision=e.grant_revision AND g.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM workflow_training_releases rel JOIN workflow_training_grants g ON g.id=rel.grant_id WHERE rel.review_id=r.id AND rel.tenant=r.tenant AND rel.project=r.project AND NOT rel.revoked AND NOT g.revoked AND rel.grant_revision=g.revision AND g.expires_at>clock_timestamp())) AND (NOT $4 OR EXISTS(SELECT 1 FROM workflow_learning_proposal_batches b WHERE b.review_id=r.id)) ORDER BY next_attempt,created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE workflow_learning_reviews r SET state='leased',fence=r.fence+1,lease_until=clock_timestamp()+make_interval(secs=>$1::double precision) FROM next WHERE r.id=next.id RETURNING r.id,r.fence,r.seal")
            .bind(f64::from(lease_seconds)).bind(scope.map(|s|s.tenant.as_str())).bind(scope.map(|s|s.project.as_str())).bind(recorded_only).fetch_optional(&mut connection).await?;
        match row {
            Some(row) => {
                let id: Uuid = row.try_get("id")?;
                let digest: String = sqlx::query_scalar(
                    "SELECT seal_content_digest FROM workflow_learning_reviews WHERE id=$1",
                )
                .bind(id)
                .fetch_one(&mut connection)
                .await?;
                Ok(Some(ClaimedReview {
                    id,
                    fence: row.try_get("fence")?,
                    seal: hydrate_review(&mut connection, row.try_get("seal")?, &digest).await?,
                }))
            }
            None => Ok(None),
        }
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
        let mut tx = self.pool.begin().await?;
        self.finish_in_transaction(&mut tx, claim, candidates, policy, ProposalOrigin::Worker)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn finish_in_transaction(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
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
        let training = self.lock_review_training(tx, claim).await?;
        let seal = sqlx::query("SELECT seal,seal_content_digest FROM workflow_learning_reviews WHERE id=$1 AND fence=$2 AND state='leased' AND lease_until>clock_timestamp() FOR UPDATE")
            .bind(claim.id).bind(claim.fence).fetch_optional(&mut tx.executor()).await?;
        let seal = seal.ok_or(RepositoryError::LeaseConflict)?;
        let digest: String = seal.try_get("seal_content_digest")?;
        let seal = hydrate_review(&mut tx.executor(), seal.try_get("seal")?, &digest).await?;
        let scope = &seal.request.scope;
        if matches!(origin, ProposalOrigin::Worker) {
            frozen::record_proposals(tx, claim.id, candidates).await?;
        }
        let approved = matches!(origin, ProposalOrigin::FrozenReplay);
        let policy = match (policy, training.publication_policy()) {
            (PublicationPolicy::Automatic, training::PublicationPolicy::HumanReviewed) => {
                PublicationPolicy::Reviewed
            }
            (policy, _) => policy,
        };
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
        self.publish_guidance_in_transaction(tx, claim.id, &seal, candidates, publish)
            .await?;
        if publish && matches!(policy, PublicationPolicy::Automatic) {
            let hashes = candidates
                .iter()
                .map(frozen::proposal_hash)
                .collect::<Result<Vec<_>>>()?;
            sqlx::query("INSERT INTO workflow_training_automatic_decisions(review_id,grant_id,candidate_hashes) VALUES($1,$2,$3) ON CONFLICT(review_id) DO NOTHING")
                .bind(claim.id).bind(training.grant_id()).bind(serde_json::to_value(hashes)?).execute(&mut tx.executor()).await?;
        }
        self.publish_candidates_in_transaction(
            tx,
            scope,
            &seal,
            candidates,
            policy,
            approved,
            &PublicationSupport::LocalReview(claim.id),
        )
        .await?;
        // Recheck real time after locks and all writes; expiry rolls back the whole publication.
        self.lock_review_training(tx, claim).await?;
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
            // The tenant publication lock is held by all publication entrypoints.
            // An identical checked teaching program may have been assigned a
            // different semantic family by an earlier proposal in this batch or
            // by a concurrent review. Attach its evidence to the active example.
            let mut conflicting_identity = false;
            if publish && candidate.verified_by(seal) {
                let duplicates = sqlx::query("SELECT id,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND variant=$3 AND identity=$4 AND state='active' ORDER BY created_at,id")
                    .bind(&scope.tenant).bind(&scope.project).bind(&candidate.variant).bind(&candidate.identity)
                    .fetch_all(&mut tx.executor()).await?;
                let mut duplicate_id = None;
                for row in duplicates {
                    let existing: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
                    if existing.checked_program() == candidate.checked_program()
                        && existing.catalog_fingerprints == candidate.catalog_fingerprints
                        && existing.behavior_fingerprint() == candidate.behavior_fingerprint()
                    {
                        duplicate_id = Some((row.try_get::<Uuid, _>("id")?, existing));
                        break;
                    }
                    if existing.family == candidate.family
                        && candidate
                            .program()
                            .zip(existing.program())
                            .is_some_and(|(new, old)| {
                                candidate.quality.strictly_better_for_same_behavior(
                                    &existing.quality,
                                    new,
                                    old,
                                )
                            })
                    {
                        continue;
                    }
                    conflicting_identity = true;
                }
                if let Some((id, existing)) = duplicate_id {
                    if let Some(reason) = support.recuration_reason(tx, candidate).await? {
                        if existing.intent != candidate.intent {
                            let incoming = &candidate.intent;
                            let has_name = |name: &String| {
                                incoming.purpose == *name || incoming.aliases.contains(name)
                            };
                            if !has_name(&existing.intent.purpose)
                                || !existing.intent.aliases.iter().all(has_name)
                                || !existing
                                    .intent
                                    .facets
                                    .iter()
                                    .all(|facet| incoming.facets.contains(facet))
                            {
                                return Err(RepositoryError::PortfolioConflict);
                            }
                            let mut revised = existing.clone();
                            revised.intent = incoming.clone();
                            let revised_id = Uuid::new_v4();
                            sqlx::query("UPDATE workflow_learning_examples SET state='superseded' WHERE tenant=$1 AND project=$2 AND id=$3 AND state='active'")
                                .bind(&scope.tenant).bind(&scope.project).bind(id).execute(&mut tx.executor()).await?;
                            sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state,reason) VALUES($1,$2,$3,$4,$5,$6,$7,'active',$8)")
                                .bind(revised_id).bind(&scope.tenant).bind(&scope.project).bind(&revised.family).bind(&revised.variant).bind(&revised.identity)
                                .bind(serde_json::to_value(&revised)?).bind(reason).execute(&mut tx.executor()).await?;
                            sqlx::query("INSERT INTO workflow_learning_support(example_id,review_id) SELECT $1,review_id FROM workflow_learning_support WHERE example_id=$2 ON CONFLICT DO NOTHING")
                                .bind(revised_id).bind(id).execute(&mut tx.executor()).await?;
                            sqlx::query("INSERT INTO workflow_learning_archive_example_support(example_id,tenant,project,archive_digest,item_digest) SELECT $1,tenant,project,archive_digest,item_digest FROM workflow_learning_archive_example_support WHERE example_id=$2 AND tenant=$3 AND project=$4 ON CONFLICT DO NOTHING")
                                .bind(revised_id).bind(id).bind(&scope.tenant).bind(&scope.project).execute(&mut tx.executor()).await?;
                            sqlx::query("UPDATE workflow_learning_families SET version=version+1 WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                                .bind(&scope.tenant).bind(&scope.project).bind(&revised.family).bind(&revised.variant).execute(&mut tx.executor()).await?;
                            support.attach_example(tx, revised_id).await?;
                            continue;
                        }
                    }
                    support.attach_example(tx, id).await?;
                    continue;
                }
            }
            // Recheck ambiguity under the publication lock. Separate workers
            // may both have compared against an empty portfolio before either
            // commits; an identical purpose and coverage are not evidence that
            // differing programs are equivalent or justify separate families.
            let mut ambiguous_cross_family = conflicting_identity;
            if publish && candidate.verified_by(seal) && !ambiguous_cross_family {
                let rows = sqlx::query("SELECT candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND variant=$3 AND family<>$4 AND identity<>$5 AND state='active'")
                    .bind(&scope.tenant).bind(&scope.project).bind(&candidate.variant).bind(&candidate.family).bind(&candidate.identity)
                    .fetch_all(&mut tx.executor()).await?;
                let purpose = super::index::normalize(&candidate.intent.purpose);
                let coverage: BTreeSet<_> = candidate.coverage.iter().collect();
                for row in rows {
                    let existing: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
                    if existing.checked_program().is_some()
                        && existing.catalog_fingerprints == candidate.catalog_fingerprints
                        && existing.coverage.iter().collect::<BTreeSet<_>>() == coverage
                        && super::index::normalize(&existing.intent.purpose) == purpose
                    {
                        ambiguous_cross_family = true;
                        break;
                    }
                }
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
            let duplicate = sqlx::query("SELECT id,state,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4 AND identity=$5 AND state <> 'superseded' AND NOT (state='bad' AND reason IS NOT DISTINCT FROM 'incompatible teaching format; retained for repair') ORDER BY created_at,id LIMIT 1")
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
                Some((_, state, old))
                    if state == "active"
                        && candidate.program().zip(old.program()).is_some_and(
                            |(new, prior)| {
                                candidate.quality.strictly_better_for_same_behavior(
                                    &old.quality,
                                    new,
                                    prior,
                                )
                            },
                        ) =>
                {
                    true
                }
                Some(_) => false,
            };
            let mut state = "candidate";
            let mut superseded_id = None;
            let mut victim: Option<(Uuid, &Quality)> = None;
            let mut equivalent_parameterized_example = false;
            if can_activate
                && publish
                && candidate.verified_by(seal)
                && !conflicting_identity
                && (!ambiguous_cross_family || approved)
            {
                let coverage: BTreeSet<_> = candidate.coverage.iter().collect();
                for (id, old) in &active {
                    if old.coverage.iter().collect::<BTreeSet<_>>() == coverage
                        && candidate
                            .checked_program()
                            .zip(old.checked_program())
                            .is_some_and(|(new, prior)| {
                                candidate.quality.equivalent_parameterized_observation(
                                    &old.quality,
                                    new,
                                    prior,
                                )
                            })
                    {
                        equivalent_parameterized_example = true;
                    }
                    if old.coverage.iter().collect::<BTreeSet<_>>() == coverage
                        && ((old.behavior_fingerprint() == candidate.behavior_fingerprint()
                            && (candidate.quality.strictly_better_than(&old.quality)
                                || candidate.program().zip(old.program()).is_some_and(
                                    |(new, prior)| {
                                        candidate.quality.strictly_better_for_same_behavior(
                                            &old.quality,
                                            new,
                                            prior,
                                        )
                                    },
                                )))
                            || candidate
                                .checked_program()
                                .zip(old.checked_program())
                                .is_some_and(|(new, prior)| {
                                    candidate
                                        .quality
                                        .strictly_better_for_parameterized_behavior(
                                            &old.quality,
                                            new,
                                            prior,
                                        )
                                }))
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
                        "UPDATE workflow_learning_examples SET state='superseded',reason='replaced by an independently observed, higher-quality equivalent teaching example' WHERE id=$1",
                    )
                    .bind(id)
                    .execute(&mut tx.executor())
                    .await?;
                    superseded_id = Some(id);
                    state = "active";
                } else if equivalent_parameterized_example {
                    // Retain the observed program and its own evidence, while
                    // serving only one equivalent parameterized presentation.
                    state = "superseded";
                } else if active.len() < 3
                    && count < self.tenant_cap
                    && !active
                        .iter()
                        .any(|(_, old)| old.coverage.iter().collect::<BTreeSet<_>>() == coverage)
                {
                    state = "active";
                }
            }
            let id = if let Some((id, _, _)) =
                existing.filter(|(id, _, _)| Some(*id) != superseded_id)
            {
                if state == "active" {
                    sqlx::query("UPDATE workflow_learning_examples SET state='active',candidate=$1,reason=NULL WHERE id=$2 AND state='candidate'")
                        .bind(serde_json::to_value(candidate)?).bind(id).execute(&mut tx.executor()).await?;
                } else if state == "superseded" {
                    sqlx::query("UPDATE workflow_learning_examples SET state='superseded',candidate=$1,reason='equivalent parameterized teaching presentation retained with its own evidence' WHERE id=$2 AND state='candidate'")
                        .bind(serde_json::to_value(candidate)?).bind(id).execute(&mut tx.executor()).await?;
                }
                id
            } else {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                    .bind(id).bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant)
                    .bind(&candidate.identity).bind(serde_json::to_value(candidate)?).bind(state).bind(candidate.diagnostic().map(|d|d.message.as_str()).or(ambiguous_cross_family.then_some("similar purpose and coverage across families require semantic review")).or((state == "superseded").then_some("equivalent parameterized teaching presentation retained with its own evidence"))).execute(&mut tx.executor()).await?;
                id
            };
            if candidate.verified_by(seal) && !conflicting_identity {
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

    /// Bounded catalogue-qualified recall for semantic comparison. Equal
    /// coverage nominates candidates; it is not a claim of equivalence.
    pub(super) async fn comparable_coverage(
        &self,
        scope: &LibraryScope,
        fingerprints: &BTreeMap<String, String>,
        coverage: &[String],
        intent: &super::index::NormalizedIntent,
        limit: u32,
    ) -> Result<Vec<StoredExample>> {
        if coverage.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query("SELECT id,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active' AND candidate->'catalog_fingerprints'=$3 AND candidate->'coverage'=$4 ORDER BY ts_rank_cd(to_tsvector('english'::regconfig,coalesce(candidate->'intent'->>'purpose','')),plainto_tsquery('english'::regconfig,$5)) DESC,created_at,id LIMIT $6")
            .bind(&scope.tenant).bind(&scope.project)
            .bind(serde_json::to_value(fingerprints)?)
            .bind(serde_json::to_value(coverage)?)
            .bind(&intent.purpose)
            .bind(i64::from(limit.min(16)))
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
