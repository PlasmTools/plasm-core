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
    pub catalogue_guidance_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenPublicationReceipt {
    pub review_ref: Uuid,
    pub artifact_hashes: Vec<String>,
    pub publication_order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicationAuthority {
    HumanOwner,
    Delegated { grant_ref: Uuid },
    HistoricalUnrecorded,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingCandidateRef {
    pub review_ref: Uuid,
    pub candidate_hash: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingCandidateDisposition {
    AwaitingDecision,
    Approved,
    Rejected,
    RevisionRequested,
    Published,
    Held,
    Incompatible,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainingCandidateSummary {
    pub reference: TrainingCandidateRef,
    pub purpose: String,
    pub disposition: TrainingCandidateDisposition,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainingCandidatePage {
    pub candidates: Vec<TrainingCandidateSummary>,
    pub next: Option<TrainingCandidateRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainingCandidateDetail {
    pub artifact: FrozenProposalArtifact,
    pub catalogue_guidance: Vec<super::super::catalogue_guidance::CatalogueAssociation>,
    pub catalogue_guidance_digest: String,
    pub supporting_executions: Vec<ExecutionSummary>,
    pub outcome_claims: Vec<AchievedSubtask>,
    pub assurance: OutcomeAssurance,
    pub source_disclosure: TrainingSourceDisclosure,
}

impl TrainingCandidateDetail {
    /// Compact agent view. Human review hydrates the full evidence separately.
    pub fn agent_view(&self) -> TrainingCandidateAgentView<'_> {
        let candidate = &self.artifact.candidate;
        TrainingCandidateAgentView {
            reference: TrainingCandidateRef {
                review_ref: self.artifact.review_ref,
                candidate_hash: self.artifact.candidate_hash.clone(),
            },
            guidance_digest: &self.catalogue_guidance_digest,
            purpose: &candidate.intent.purpose,
            source: candidate.program().map(|program| program.source.as_str()),
            catalogue_guidance: self
                .catalogue_guidance
                .iter()
                .map(|item| item.intent().purpose.as_str())
                .collect(),
            supporting_execution_count: self.supporting_executions.len(),
            outcomes: self
                .outcome_claims
                .iter()
                .map(|claim| AgentOutcomeClaim {
                    intent: &claim.intent,
                    observed_outcome: &claim.observed_outcome,
                })
                .collect(),
            assurance: &self.assurance,
            source_disclosure: &self.source_disclosure,
        }
    }
}

#[derive(Serialize)]
pub struct TrainingCandidateAgentView<'a> {
    reference: TrainingCandidateRef,
    guidance_digest: &'a str,
    purpose: &'a str,
    source: Option<&'a str>,
    catalogue_guidance: Vec<&'a str>,
    supporting_execution_count: usize,
    outcomes: Vec<AgentOutcomeClaim<'a>>,
    assurance: &'a OutcomeAssurance,
    source_disclosure: &'a TrainingSourceDisclosure,
}

#[derive(Serialize)]
struct AgentOutcomeClaim<'a> {
    intent: &'a str,
    observed_outcome: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingSourceDisclosure {
    Available,
    WithheldSensitive,
}

fn candidate_requires_redaction(candidate: &Candidate) -> bool {
    candidate.diagnostic().is_some_and(|diagnostic| {
        matches!(
            diagnostic.kind,
            DiagnosticKind::SensitiveOperand | DiagnosticKind::DisclosureUncertainty
        )
    })
}

/// Presentation only. The original proposal bytes and revision hash stay stored.
fn redact_candidate_presentation(candidate: &mut Candidate) {
    if !candidate_requires_redaction(candidate) {
        return;
    }
    candidate.intent.purpose = "Sensitive candidate withheld".into();
    candidate.intent.facets.clear();
    candidate.intent.aliases.clear();
    candidate.coverage.clear();
    candidate.family = "withheld".into();
    candidate.variant = "withheld".into();
    candidate.identity = "withheld".into();
    candidate.embedding_profile = "withheld".into();
    candidate.state = CandidateState::Proposed {
        diagnostic: ProposalDiagnostic {
            kind: DiagnosticKind::SensitiveOperand,
            message: "Sensitive source and semantic excerpts withheld from review presentation"
                .into(),
        },
    };
}

/// CAS is a publication precondition, not part of a proposal's semantic identity.
/// Stored payloads retain their original value; replay updates only its local copy.
pub(crate) fn proposal_hash(candidate: &Candidate) -> Result<String> {
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
    pub async fn training_candidate_session(
        &self,
        scope: &LibraryScope,
        reference: &TrainingCandidateRef,
    ) -> Result<LogicalSessionId> {
        let mut connection = self.pool.acquire().await?;
        let logical: Option<String> = sqlx::query_scalar("SELECT r.logical_session FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id WHERE r.tenant=$1 AND r.project=$2 AND r.id=$3 AND p.candidate_hash=$4")
            .bind(&scope.tenant).bind(&scope.project).bind(reference.review_ref).bind(&reference.candidate_hash).fetch_optional(&mut connection).await?;
        Ok(LogicalSessionId(
            logical
                .ok_or(RepositoryError::Unavailable)?
                .parse()
                .map_err(|_| RepositoryError::InvalidEvidence("invalid stored logical session"))?,
        ))
    }

    pub async fn list_training_candidates(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
        after: Option<&TrainingCandidateRef>,
        limit: u32,
    ) -> Result<TrainingCandidatePage> {
        if !(1..=32).contains(&limit) {
            return Err(RepositoryError::InvalidEvidence(
                "candidate page limit must be 1..32",
            ));
        }
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query("SELECT r.id,p.candidate_hash,p.candidate,r.state,(SELECT d.action FROM workflow_training_candidate_decisions d WHERE d.review_id=r.id AND d.candidate_hash=p.candidate_hash ORDER BY d.decided_at DESC LIMIT 1) AS decision,EXISTS(SELECT 1 FROM workflow_learning_examples e JOIN workflow_learning_support s ON s.example_id=e.id WHERE s.review_id=r.id AND e.tenant=r.tenant AND e.project=r.project AND e.identity=p.candidate->>'identity' AND e.variant=p.candidate->>'variant' AND e.candidate#>'{state,evidence,program}'=p.candidate#>'{state,evidence,program}' AND e.state='active') AS published,EXISTS(SELECT 1 FROM workflow_training_epochs ep WHERE ep.id=r.training_epoch AND ep.held AND NOT EXISTS(SELECT 1 FROM workflow_training_releases rel JOIN workflow_training_grants g ON g.id=rel.grant_id WHERE rel.review_id=r.id AND NOT rel.revoked AND NOT g.revoked AND g.expires_at>clock_timestamp() AND g.revision=rel.grant_revision)) AS held FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id WHERE r.tenant=$1 AND r.project=$2 AND r.logical_session=$3 AND ($4::uuid IS NULL OR (r.id,p.candidate_hash)>($4,$5)) ORDER BY r.id,p.candidate_hash LIMIT $6")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string())
            .bind(after.map(|cursor|cursor.review_ref)).bind(after.map(|cursor|cursor.candidate_hash.as_str()))
            .bind(i64::from(limit)+1).fetch_all(&mut connection).await?;
        let has_more = rows.len() > limit as usize;
        let candidates = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| {
                let mut candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
                redact_candidate_presentation(&mut candidate);
                let decision: Option<serde_json::Value> = row.try_get("decision")?;
                let disposition = if row.try_get::<bool, _>("published")? {
                    TrainingCandidateDisposition::Published
                } else if row.try_get::<bool, _>("held")? {
                    TrainingCandidateDisposition::Held
                } else if let Some(value) = decision {
                    match serde_json::from_value::<training::CandidateDecisionAction>(value)? {
                        training::CandidateDecisionAction::Approve => {
                            TrainingCandidateDisposition::Approved
                        }
                        training::CandidateDecisionAction::Reject { .. } => {
                            TrainingCandidateDisposition::Rejected
                        }
                        training::CandidateDecisionAction::RequestRevision { .. } => {
                            TrainingCandidateDisposition::RevisionRequested
                        }
                    }
                } else {
                    TrainingCandidateDisposition::AwaitingDecision
                };
                Ok(TrainingCandidateSummary {
                    reference: TrainingCandidateRef {
                        review_ref: row.try_get("id")?,
                        candidate_hash: row.try_get("candidate_hash")?,
                    },
                    purpose: candidate.intent.purpose,
                    disposition,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next = has_more.then(|| {
            candidates
                .last()
                .expect("nonempty limited page")
                .reference
                .clone()
        });
        Ok(TrainingCandidatePage { candidates, next })
    }

    pub async fn read_training_candidate(
        &self,
        scope: &LibraryScope,
        reference: &TrainingCandidateRef,
    ) -> Result<TrainingCandidateDetail> {
        let mut connection = self.pool.acquire().await?;
        let row = sqlx::query("SELECT r.digest,r.seal,r.seal_content_digest,p.candidate,coalesce(g.associations,'[]'::jsonb) AS guidance FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id LEFT JOIN workflow_learning_guidance_batches g ON g.review_id=r.id WHERE r.tenant=$1 AND r.project=$2 AND r.id=$3 AND p.candidate_hash=$4")
            .bind(&scope.tenant).bind(&scope.project).bind(reference.review_ref).bind(&reference.candidate_hash)
            .fetch_optional(&mut connection).await?.ok_or(RepositoryError::Unavailable)?;
        let mut candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
        if proposal_hash(&candidate)? != reference.candidate_hash {
            return Err(RepositoryError::ImmutableConflict);
        }
        let seal = hydrate_review(
            &mut connection,
            row.try_get("seal")?,
            &row.try_get::<String, _>("seal_content_digest")?,
        )
        .await?;
        let sensitive = candidate_requires_redaction(&candidate);
        let supporting_executions: Vec<_> = seal
            .executions
            .iter()
            .filter(|receipt| {
                super::super::teaching::CheckedTeachingExample::from_observation(
                    &receipt.observation,
                )
                .is_ok_and(|program| candidate.checked_program() == Some(&program))
            })
            .map(|receipt| ExecutionSummary {
                execution_id: receipt.execution_id,
                session_revision: receipt.session_revision,
            })
            .collect();
        let outcome_claims = if sensitive {
            Vec::new()
        } else {
            seal.request
                .subtasks
                .iter()
                .filter_map(|claim| {
                    let execution_ids = claim
                        .execution_ids
                        .iter()
                        .filter(|id| {
                            supporting_executions
                                .iter()
                                .any(|run| &run.execution_id == *id)
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    (!execution_ids.is_empty()).then(|| AchievedSubtask {
                        intent: claim.intent.clone(),
                        observed_outcome: claim.observed_outcome.clone(),
                        execution_ids,
                    })
                })
                .collect()
        };
        redact_candidate_presentation(&mut candidate);
        let guidance: serde_json::Value = row.try_get("guidance")?;
        Ok(TrainingCandidateDetail {
            artifact: FrozenProposalArtifact {
                review_ref: reference.review_ref,
                review_digest: row.try_get("digest")?,
                candidate_hash: reference.candidate_hash.clone(),
                candidate,
            },
            catalogue_guidance_digest: plasm_core::catalog_discovery::content_hash(
                &serde_json::to_vec(&guidance)?,
            ),
            catalogue_guidance: if sensitive {
                Vec::new()
            } else {
                serde_json::from_value(guidance)?
            },
            supporting_executions,
            outcome_claims,
            assurance: seal.assurance,
            source_disclosure: if sensitive {
                TrainingSourceDisclosure::WithheldSensitive
            } else {
                TrainingSourceDisclosure::Available
            },
        })
    }

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
        expected_guidance_digest: &str,
        actor: &training::TrustedTrainingActor,
    ) -> Result<FrozenPublicationReceipt> {
        let mut tx = self.pool.begin().await?;
        let receipt = self
            .publish_frozen_authorized(
                &mut tx,
                scope,
                review_id,
                expected_hashes,
                expected_guidance_digest,
                (actor, None),
            )
            .await?;
        tx.commit().await?;
        Ok(receipt)
    }

    pub(super) async fn publish_frozen_authorized(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        scope: &LibraryScope,
        review_id: Uuid,
        expected_hashes: &[String],
        expected_guidance_digest: &str,
        authorization: (&training::TrustedTrainingActor, Option<Uuid>),
    ) -> Result<FrozenPublicationReceipt> {
        let (actor, decision_request) = authorization;
        if actor.scope() != scope || expected_hashes.is_empty() || expected_hashes.len() > 32 {
            return Err(RepositoryError::InvalidEvidence(
                "bounded frozen hashes and scoped publication authority are required",
            ));
        }
        let mut expected = expected_hashes.to_vec();
        expected.sort();
        if expected.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RepositoryError::ImmutableConflict);
        }
        let logical: Option<String> = sqlx::query_scalar("SELECT logical_session FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?;
        let logical = LogicalSessionId(
            logical
                .ok_or(RepositoryError::Unavailable)?
                .parse()
                .map_err(|_| RepositoryError::InvalidEvidence("invalid stored logical session"))?,
        );
        let epoch: Option<Uuid> = sqlx::query_scalar("SELECT training_epoch FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?.flatten();
        let publication_permit = training::authorize_review_publication(
            tx,
            scope,
            logical,
            review_id,
            epoch.ok_or(training::TrainingError::Inactive)?,
            actor,
        )
        .await?;
        let authority = if actor.is_human_owner() {
            PublicationAuthority::HumanOwner
        } else {
            PublicationAuthority::Delegated {
                grant_ref: publication_permit.grant_id(),
            }
        };
        if let Some(request) = decision_request {
            let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_training_requests WHERE id=$1 AND tenant=$2 AND project=$3 AND state='approved')")
                .bind(request).bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
            if !active {
                return Err(training::TrainingError::Held.into());
            }
        }
        let row=sqlx::query("SELECT state,seal,digest,seal_content_digest FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 FOR UPDATE")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?.ok_or(RepositoryError::ApprovalUnavailable)?;
        let payload: serde_json::Value = row.try_get("seal")?;
        // Migration may already have failed this job. Retain the precise
        // incompatibility diagnostic instead of obscuring it as a state error.
        validate_seal_format(&payload)?;
        let state: String = row.try_get("state")?;
        if state != "completed" && state != "awaiting_approval" {
            return Err(RepositoryError::ApprovalUnavailable);
        }
        let seal = hydrate_review(
            &mut tx.executor(),
            payload,
            &row.try_get::<String, _>("seal_content_digest")?,
        )
        .await?;
        if seal.request.scope != *scope {
            return Err(RepositoryError::ImmutableConflict);
        }
        let recorded: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT artifact_hashes FROM workflow_learning_proposal_batches WHERE review_id=$1",
        )
        .bind(review_id)
        .fetch_optional(&mut tx.executor())
        .await?;
        let recorded: Vec<String> =
            serde_json::from_value(recorded.ok_or(RepositoryError::Unavailable)?)?;
        if !expected.iter().all(|hash| recorded.contains(hash)) {
            return Err(RepositoryError::ImmutableConflict);
        }
        let guidance: serde_json::Value = sqlx::query_scalar(
            "SELECT associations FROM workflow_learning_guidance_batches WHERE review_id=$1",
        )
        .bind(review_id)
        .fetch_optional(&mut tx.executor())
        .await?
        .unwrap_or_else(|| serde_json::json!([]));
        let guidance_digest =
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&guidance)?);
        if guidance_digest != expected_guidance_digest {
            return Err(RepositoryError::ImmutableConflict);
        }
        let selection_digest = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
            &(&expected, &guidance_digest),
        )?);
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
            if !expected.contains(&stored_hash) {
                continue;
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
        if let Some(previous)=sqlx::query("SELECT actor,credential,artifact_hashes,publication_order FROM workflow_training_publications WHERE review_id=$1 AND tenant=$2 AND project=$3 AND selection_digest=$4")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).bind(&selection_digest).fetch_optional(&mut tx.executor()).await? {
            if previous.try_get::<String,_>("actor")?!=actor.subject() || previous.try_get::<String,_>("credential")?!=actor.credential() || previous.try_get::<serde_json::Value,_>("artifact_hashes")?!=serde_json::to_value(&expected)? {return Err(RepositoryError::ImmutableConflict);}
            return Ok(FrozenPublicationReceipt{review_ref:review_id,artifact_hashes:expected,publication_order:previous.try_get("publication_order")?});
        }
        for candidate in &mut candidates {
            candidate.expected_family_version=sqlx::query_scalar("SELECT version FROM workflow_learning_families WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).fetch_optional(&mut tx.executor()).await?.unwrap_or(0);
        }
        let publication_order:i64=sqlx::query_scalar("INSERT INTO workflow_training_publications(review_id,tenant,project,actor,credential,artifact_hashes,guidance_digest,selection_digest,authority) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING publication_order")
            .bind(review_id).bind(&scope.tenant).bind(&scope.project).bind(actor.subject()).bind(actor.credential()).bind(serde_json::to_value(&expected)?).bind(&guidance_digest).bind(&selection_digest).bind(serde_json::to_value(authority)?).fetch_one(&mut tx.executor()).await?;
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
