//! Separate bounded catalogue guidance, published within the review completion transaction.
use super::*;
use crate::workflow_learning::{catalogue_guidance::CatalogueAssociation, index};

/// Host-only inspection binds every learning product to one immutable review.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FrozenReviewArtifacts {
    Compatible {
        review_ref: Uuid,
        review_digest: String,
        examples: Vec<super::frozen::FrozenProposalArtifact>,
        catalogue_guidance: Vec<CatalogueAssociation>,
        catalogue_guidance_digest: String,
    },
    Incompatible {
        review_ref: Uuid,
        review_digest: String,
        reason: ReviewInspectionIncompatibility,
    },
}
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewInspectionIncompatibility {
    IncompatibleTeachingFormat,
}

pub struct GuidanceSearch<'a> {
    pub intent: &'a index::NormalizedIntent,
    pub embedding: &'a [f32],
    pub profile: &'a str,
    pub allowed: &'a crate::discovery_store::DiscoveryAuthorization,
    pub catalog_fingerprints: &'a BTreeMap<String, String>,
    pub limit: usize,
}
impl WorkflowRepository {
    pub async fn export_review_artifacts(
        &self,
        scope: &LibraryScope,
    ) -> Result<Vec<FrozenReviewArtifacts>> {
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query("SELECT r.id,r.digest,r.seal,coalesce(g.associations,'[]'::jsonb) AS guidance,coalesce((SELECT jsonb_agg(jsonb_build_object('candidate_hash',p.candidate_hash,'candidate',p.candidate) ORDER BY p.candidate_hash) FROM workflow_learning_proposals p WHERE p.review_id=r.id),'[]'::jsonb) AS examples FROM workflow_learning_reviews r LEFT JOIN workflow_learning_guidance_batches g ON g.review_id=r.id WHERE r.tenant=$1 AND r.project=$2 AND (g.review_id IS NOT NULL OR EXISTS(SELECT 1 FROM workflow_learning_proposal_batches b WHERE b.review_id=r.id)) ORDER BY r.created_at,r.id")
            .bind(scope.tenant.clone()).bind(scope.project.clone()).fetch_all(&mut connection).await?;
        rows.into_iter()
            .map(|row| {
                let id: Uuid = row.try_get("id")?;
                let digest: String = row.try_get("digest")?;
                let seal_value: serde_json::Value = row.try_get("seal")?;
                let incompatible = || FrozenReviewArtifacts::Incompatible {
                    review_ref: id,
                    review_digest: digest.clone(),
                    reason: ReviewInspectionIncompatibility::IncompatibleTeachingFormat,
                };
                if validate_seal_format(&seal_value).is_err() {
                    return Ok(incompatible());
                }
                let seal: SealedReview = match serde_json::from_value(seal_value) {
                    Ok(seal) => seal,
                    Err(_) => return Ok(incompatible()),
                };
                if seal.request.scope != *scope {
                    return Err(RepositoryError::ImmutableConflict);
                }
                let guidance_value: serde_json::Value = row.try_get("guidance")?;
                let guidance_digest = plasm_core::catalog_discovery::content_hash(
                    &serde_json::to_vec(&guidance_value)?,
                );
                let guidance: Vec<CatalogueAssociation> = serde_json::from_value(guidance_value)?;
                for association in &guidance {
                    association.validate(&seal)?;
                }
                let examples_value: serde_json::Value = row.try_get("examples")?;
                let mut examples = Vec::new();
                for artifact in examples_value
                    .as_array()
                    .ok_or(RepositoryError::ImmutableConflict)?
                {
                    let hash = artifact["candidate_hash"]
                        .as_str()
                        .ok_or(RepositoryError::ImmutableConflict)?;
                    let candidate: Candidate =
                        match serde_json::from_value(artifact["candidate"].clone()) {
                            Ok(candidate) => candidate,
                            Err(_) => return Ok(incompatible()),
                        };
                    if super::frozen::proposal_hash(&candidate)? != hash {
                        return Err(RepositoryError::ImmutableConflict);
                    }
                    examples.push(super::frozen::FrozenProposalArtifact {
                        review_ref: id,
                        review_digest: digest.clone(),
                        candidate_hash: hash.into(),
                        candidate,
                    });
                }
                Ok(FrozenReviewArtifacts::Compatible {
                    review_ref: id,
                    review_digest: digest,
                    examples,
                    catalogue_guidance: guidance,
                    catalogue_guidance_digest: guidance_digest,
                })
            })
            .collect()
    }

    pub async fn has_active_learning(&self, scope: &LibraryScope) -> Result<bool> {
        let mut connection = self.pool.acquire().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active') OR EXISTS(SELECT 1 FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2)")
            .bind(scope.tenant.clone()).bind(scope.project.clone()).fetch_one(&mut connection).await?)
    }

    pub async fn finish_with_guidance(
        &self,
        claim: &ClaimedReview,
        candidates: &[Candidate],
        associations: &[CatalogueAssociation],
        policy: PublicationPolicy,
    ) -> Result<()> {
        if associations.len() > 32 {
            return Err(RepositoryError::InvalidEvidence(
                "guidance batch exceeds 32",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let seal: Option<serde_json::Value> = sqlx::query_scalar("SELECT seal FROM workflow_learning_reviews WHERE id=$1 AND fence=$2 AND state='leased' AND lease_until>clock_timestamp() FOR UPDATE")
            .bind(claim.id).bind(claim.fence).fetch_optional(&mut tx.executor()).await?;
        let seal: SealedReview =
            serde_json::from_value(seal.ok_or(RepositoryError::LeaseConflict)?)?;
        if seal != claim.seal {
            return Err(RepositoryError::ImmutableConflict);
        }
        let mut payloads = BTreeMap::new();
        for association in associations {
            association.validate(&seal)?;
            let value = serde_json::to_value(association)?;
            payloads.insert(
                plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&value)?),
                value,
            );
        }
        let payload = serde_json::Value::Array(payloads.into_values().collect());
        let stored: serde_json::Value = sqlx::query_scalar("INSERT INTO workflow_learning_guidance_batches(review_id,associations) VALUES($1,$2) ON CONFLICT(review_id) DO UPDATE SET review_id=EXCLUDED.review_id RETURNING associations")
            .bind(claim.id).bind(&payload).fetch_one(&mut tx.executor()).await?;
        if stored != payload {
            return Err(RepositoryError::ImmutableConflict);
        }
        self.finish_in_transaction(tx, claim, candidates, policy, ProposalOrigin::Worker)
            .await
    }

    pub async fn frozen_guidance(
        &self,
        scope: &LibraryScope,
        review: Uuid,
    ) -> Result<Option<Vec<CatalogueAssociation>>> {
        let mut connection = self.pool.acquire().await?;
        let payload: Option<serde_json::Value> = sqlx::query_scalar("SELECT b.associations FROM workflow_learning_guidance_batches b JOIN workflow_learning_reviews r ON r.id=b.review_id WHERE r.id=$1 AND r.tenant=$2 AND r.project=$3")
            .bind(review).bind(scope.tenant.clone()).bind(scope.project.clone()).fetch_optional(&mut connection).await?;
        payload
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    /// Caller holds the tenant lock and the fenced review lock. This shares the
    /// example transaction, including rollback and reviewed publication approval.
    pub(super) async fn publish_guidance_in_transaction(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        review: Uuid,
        seal: &SealedReview,
        publish: bool,
    ) -> Result<()> {
        let payload: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT associations FROM workflow_learning_guidance_batches WHERE review_id=$1",
        )
        .bind(review)
        .fetch_optional(&mut tx.executor())
        .await?;
        let Some(payload) = payload else {
            return Ok(());
        };
        let associations: Vec<CatalogueAssociation> = serde_json::from_value(payload)?;
        let scope = &seal.request.scope;
        for association in associations {
            association.validate(seal)?;
            if !publish || !association.confirmed {
                continue;
            }
            let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(&association.family).bind(&association.variant).fetch_optional(&mut tx.executor()).await?;
            let id = if let Some(id) = existing {
                id
            } else {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM workflow_learning_catalogue_guidance WHERE tenant=$1",
                )
                .bind(&scope.tenant)
                .fetch_one(&mut tx.executor())
                .await?;
                if count >= self.tenant_cap {
                    continue;
                }
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO workflow_learning_catalogue_guidance(id,tenant,project,family,variant,association) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind(id).bind(&scope.tenant).bind(&scope.project).bind(&association.family).bind(&association.variant)
                    .bind(serde_json::to_value(&association)?).execute(&mut tx.executor()).await?;
                id
            };
            sqlx::query("INSERT INTO workflow_learning_guidance_support(association_id,review_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
                .bind(id).bind(review).execute(&mut tx.executor()).await?;
        }
        Ok(())
    }

    /// The tenant-wide bound makes this full scoped snapshot bounded. Capability
    /// authorization and exact effective catalogue pins precede ranking/disclosure.
    pub async fn search_guidance(
        &self,
        scope: &LibraryScope,
        query: GuidanceSearch<'_>,
    ) -> Result<Vec<CatalogueAssociation>> {
        let mut connection = self.pool.acquire().await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar("SELECT association FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2 AND association->>'embedding_profile'=$3 ORDER BY id LIMIT $4")
            .bind(scope.tenant.clone()).bind(scope.project.clone()).bind(query.profile.to_owned()).bind(self.tenant_cap).fetch_all(&mut connection).await?;
        let mut allowed = Vec::new();
        for row in rows {
            let association: CatalogueAssociation = serde_json::from_value(row)?;
            if association
                .requirements
                .catalog_fingerprints()
                .iter()
                .all(|(id, hash)| {
                    query.allowed.catalogs.contains(id)
                        && query.catalog_fingerprints.get(id) == Some(hash)
                })
                && association
                    .requirements
                    .required_capabilities()
                    .iter()
                    .all(|cap| query.allowed.permits(cap))
            {
                allowed.push(association);
            }
        }
        let entries = allowed
            .iter()
            .enumerate()
            .map(|(id, a)| index::IndexEntry {
                id: id.to_string(),
                intent: a.intent.clone(),
                catalogs: a
                    .requirements
                    .catalog_fingerprints()
                    .keys()
                    .cloned()
                    .collect(),
                embedding: Some(a.embedding.clone()),
            })
            .collect::<Vec<_>>();
        Ok(index::rank(
            query.intent,
            Some(query.embedding),
            &query.allowed.catalogs,
            None,
            &entries,
            query.limit.min(3),
        )
        .into_iter()
        .filter_map(|rank| {
            rank.id
                .parse::<usize>()
                .ok()
                .and_then(|id| allowed.get(id).cloned())
        })
        .collect())
    }
}

#[cfg(test)]
#[path = "catalogue_guidance_tests.rs"]
mod tests;
