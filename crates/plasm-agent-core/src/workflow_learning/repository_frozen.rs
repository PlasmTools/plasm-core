//! Immutable proposal audit and host-only deterministic publication from frozen
//! artifacts. The ordinary portfolio transaction remains the sole publisher.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenProposalArtifact {
    pub review_ref: Uuid,
    pub review_digest: String,
    pub candidate_hash: String,
    pub candidate: Candidate,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenPublicationRequest {
    pub review_ref: Uuid,
    pub artifact_hashes: Vec<String>,
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenPublicationReceipt {
    pub review_ref: Uuid,
    pub artifact_hashes: Vec<String>,
    pub publication_order: i64,
}

/// CAS is a publication precondition, not part of a proposal's semantic identity.
/// Stored payloads retain their original value; replay updates only its local copy.
pub(super) fn proposal_hash(candidate: &Candidate) -> Result<String> {
    let mut canonical = candidate.clone();
    canonical.expected_family_version = 0;
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&canonical)?)))
}

pub(super) async fn record_proposals(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    review: Uuid,
    candidates: &[Candidate],
) -> Result<()> {
    let mut artifacts = BTreeMap::new();
    for candidate in candidates {
        artifacts.insert(proposal_hash(candidate)?, serde_json::to_value(candidate)?);
    }
    let hashes = serde_json::to_value(artifacts.keys().collect::<Vec<_>>())?;
    let stored:serde_json::Value=sqlx::query_scalar("INSERT INTO workflow_learning_proposal_batches(review_id,artifact_hashes) VALUES($1,$2) ON CONFLICT(review_id) DO UPDATE SET review_id=EXCLUDED.review_id RETURNING artifact_hashes")
        .bind(review).bind(&hashes).fetch_one(&mut tx.executor()).await?;
    if stored != hashes {
        return Err(RepositoryError::ImmutableConflict);
    }
    for (hash, payload) in artifacts {
        let stored:serde_json::Value=sqlx::query_scalar("INSERT INTO workflow_learning_proposals(review_id,candidate_hash,candidate) VALUES($1,$2,$3) ON CONFLICT(review_id,candidate_hash) DO UPDATE SET candidate_hash=EXCLUDED.candidate_hash RETURNING candidate")
            .bind(review).bind(&hash).bind(payload).fetch_one(&mut tx.executor()).await?;
        if proposal_hash(&serde_json::from_value(stored)?)? != hash {
            return Err(RepositoryError::ImmutableConflict);
        }
    }
    Ok(())
}

impl WorkflowRepository {
    /// Recover the original semantic proposals for an approved/retried review.
    /// Only optimistic concurrency preconditions are refreshed.
    pub(in crate::workflow_learning) async fn frozen_candidates(
        &self,
        scope: &LibraryScope,
        review: Uuid,
    ) -> Result<Option<Vec<Candidate>>> {
        let mut connection = self.pool.acquire().await?;
        let hashes: Option<serde_json::Value> = sqlx::query_scalar("SELECT b.artifact_hashes FROM workflow_learning_proposal_batches b JOIN workflow_learning_reviews r ON r.id=b.review_id WHERE r.id=$1 AND r.tenant=$2 AND r.project=$3")
            .bind(review).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut connection).await?;
        let Some(hashes) = hashes else {
            return Ok(None);
        };
        let expected: Vec<String> = serde_json::from_value(hashes)?;
        let rows = sqlx::query("SELECT candidate_hash,candidate FROM workflow_learning_proposals WHERE review_id=$1 ORDER BY candidate_hash")
            .bind(review).fetch_all(&mut connection).await?;
        drop(connection);
        let mut actual = Vec::with_capacity(rows.len());
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let mut candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
            let hash: String = row.try_get("candidate_hash")?;
            if proposal_hash(&candidate)? != hash {
                return Err(RepositoryError::ImmutableConflict);
            }
            actual.push(hash);
            candidate.expected_family_version = self
                .portfolio_version(scope, &candidate.family, &candidate.variant)
                .await?;
            candidates.push(candidate);
        }
        if actual != expected {
            return Err(RepositoryError::ImmutableConflict);
        }
        Ok(Some(candidates))
    }

    /// Includes every recorded proposal, not just eligible/support-linked examples.
    pub async fn export_proposals(
        &self,
        scope: &LibraryScope,
    ) -> Result<Vec<FrozenProposalArtifact>> {
        let mut connection = self.pool.acquire().await?;
        let rows=sqlx::query("SELECT r.id,r.digest,p.candidate_hash,p.candidate FROM workflow_learning_proposals p JOIN workflow_learning_reviews r ON r.id=p.review_id WHERE r.tenant=$1 AND r.project=$2 ORDER BY r.created_at,r.id,p.candidate_hash")
            .bind(&scope.tenant).bind(&scope.project).fetch_all(&mut connection).await?;
        rows.into_iter()
            .map(|row| {
                let candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
                let candidate_hash: String = row.try_get("candidate_hash")?;
                if proposal_hash(&candidate)? != candidate_hash {
                    return Err(RepositoryError::ImmutableConflict);
                }
                Ok(FrozenProposalArtifact {
                    review_ref: row.try_get("id")?,
                    review_digest: row.try_get("digest")?,
                    candidate_hash,
                    candidate,
                })
            })
            .collect()
    }

    /// Replays the complete immutable batch through existing Reviewed gates, with
    /// no provider calls and no direct changes to example activation state.
    pub async fn publish_frozen(
        &self,
        scope: &LibraryScope,
        review_id: Uuid,
        expected_hashes: &[String],
        actor: &str,
    ) -> Result<FrozenPublicationReceipt> {
        if actor.trim().is_empty() || expected_hashes.len() > 32 {
            return Err(RepositoryError::InvalidEvidence(
                "bounded frozen hashes and approval actor are required",
            ));
        }
        let mut expected = expected_hashes.to_vec();
        expected.sort();
        if expected.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RepositoryError::ImmutableConflict);
        }
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT state,seal,digest FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 FOR UPDATE")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?.ok_or(RepositoryError::ApprovalUnavailable)?;
        let payload: serde_json::Value = row.try_get("seal")?;
        // Migration may already have failed this job. Retain the precise
        // incompatibility diagnostic instead of obscuring it as a state error.
        validate_seal_format(&payload)?;
        let state: String = row.try_get("state")?;
        if state != "completed" && state != "awaiting_approval" {
            return Err(RepositoryError::ApprovalUnavailable);
        }
        let seal: SealedReview = serde_json::from_value(payload)?;
        if seal.request.scope != *scope {
            return Err(RepositoryError::ImmutableConflict);
        }
        let recorded: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT artifact_hashes FROM workflow_learning_proposal_batches WHERE review_id=$1",
        )
        .bind(review_id)
        .fetch_optional(&mut tx.executor())
        .await?;
        if recorded != Some(serde_json::to_value(&expected)?) {
            return Err(RepositoryError::ImmutableConflict);
        }
        // Match finish's review→tenant lock order before reading CAS versions.
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
        let rows=sqlx::query("SELECT candidate_hash,candidate FROM workflow_learning_proposals WHERE review_id=$1 ORDER BY candidate_hash")
            .bind(review_id).fetch_all(&mut tx.executor()).await?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
            let stored_hash: String = row.try_get("candidate_hash")?;
            if proposal_hash(&candidate)? != stored_hash {
                return Err(RepositoryError::ImmutableConflict);
            }
            let states:Vec<String>=sqlx::query_scalar("SELECT state FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4 AND identity=$5 AND NOT (state='bad' AND reason IS NOT DISTINCT FROM 'incompatible teaching format; retained for repair') FOR SHARE")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).bind(&candidate.identity).fetch_all(&mut tx.executor()).await?;
            if states
                .iter()
                .any(|state| state == "bad" || state == "superseded")
            {
                return Err(RepositoryError::ApprovalUnavailable);
            }
            candidates.push(candidate);
        }
        if candidates.len() != expected.len() {
            return Err(RepositoryError::ImmutableConflict);
        }
        if let Some(previous)=sqlx::query("SELECT actor,artifact_hashes,publication_order FROM workflow_learning_frozen_publications WHERE review_id=$1 AND tenant=$2 AND project=$3")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await? {
            if previous.try_get::<String,_>("actor")?!=actor || previous.try_get::<serde_json::Value,_>("artifact_hashes")?!=serde_json::to_value(&expected)? {return Err(RepositoryError::ImmutableConflict);}
            return Ok(FrozenPublicationReceipt{review_ref:review_id,artifact_hashes:expected,publication_order:previous.try_get("publication_order")?});
        }
        for candidate in &mut candidates {
            candidate.expected_family_version=sqlx::query_scalar("SELECT version FROM workflow_learning_families WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).fetch_optional(&mut tx.executor()).await?.unwrap_or(0);
        }
        sqlx::query("INSERT INTO workflow_learning_publication_approvals(review_id,tenant,project,actor) VALUES($1,$2,$3,$4) ON CONFLICT(review_id) DO NOTHING")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).bind(actor).execute(&mut tx.executor()).await?;
        let publication_order:i64=sqlx::query_scalar("INSERT INTO workflow_learning_frozen_publications(review_id,tenant,project,actor,artifact_hashes) VALUES($1,$2,$3,$4,$5) RETURNING publication_order")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).bind(actor).bind(serde_json::to_value(&expected)?).fetch_one(&mut tx.executor()).await?;
        let fence:i64=sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=fence+1,lease_until=clock_timestamp()+interval '300 seconds' WHERE id=$1 RETURNING fence")
            .bind(review_id).fetch_one(&mut tx.executor()).await?;
        let claim = ClaimedReview {
            id: review_id,
            fence,
            seal,
        };
        self.finish_in_transaction(
            tx,
            &claim,
            &candidates,
            PublicationPolicy::Reviewed,
            ProposalOrigin::FrozenReplay,
        )
        .await?;
        Ok(FrozenPublicationReceipt {
            review_ref: review_id,
            artifact_hashes: expected,
            publication_order,
        })
    }
}

#[cfg(test)]
#[path = "repository_frozen_tests.rs"]
mod tests;
