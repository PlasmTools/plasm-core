//! Human decisions bind immutable selections; publication remains independently gated.
use super::*;
use serde::{Deserialize, Serialize};
use training::{CandidateDecisionAction, TrustedTrainingActor};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingDecisionReceipt {
    pub request_ref: Uuid,
    pub decisions: Vec<TrainingCandidateDecisionReceipt>,
    pub publications: Vec<FrozenPublicationReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingCandidateDecisionReceipt {
    pub reference: TrainingCandidateRef,
    pub action: CandidateDecisionAction,
    pub outcome_confirmation: training::HumanOutcomeConfirmation,
    pub outcome: CandidateDecisionOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CandidateDecisionOutcome {
    DecisionDeclined,
    Rejected,
    RevisionRequested,
    ApprovedPendingPublication,
    Published,
    PublicationDeferred { reason: PublicationDeferral },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationDeferral {
    Held,
    Incompatible,
    AdmissionDeclined,
    Conflict,
    Infrastructure,
}

impl WorkflowRepository {
    /// Acceptance and bounded publication share the epoch/portfolio transaction.
    /// A process crash cannot leave an accepted decision without its final receipt.
    pub async fn resolve_training_candidate_request(
        &self,
        actor: &TrustedTrainingActor,
        request_ref: Uuid,
        request_revision: u64,
        idempotency_key: Uuid,
        selected: &[training::TrainingCandidateSelection],
        approve: bool,
    ) -> Result<TrainingDecisionReceipt> {
        if !actor.is_human_owner() {
            return Err(training::TrainingError::Denied.into());
        }
        let mut selected = selected.to_vec();
        selected.sort_by(|left, right| {
            (left.review_ref, &left.candidate_hash).cmp(&(right.review_ref, &right.candidate_hash))
        });
        if (approve && selected.is_empty()) || selected.len()>32 || selected.windows(2).any(|pair|pair[0].review_ref==pair[1].review_ref && pair[0].candidate_hash==pair[1].candidate_hash)
            || selected.iter().any(|item|matches!(&item.action,CandidateDecisionAction::Reject{reason}|CandidateDecisionAction::RequestRevision{reason} if reason.trim().is_empty() || reason.len()>4096)) {
            return Err(training::TrainingError::InvalidCommand.into());
        }
        let selection_json = serde_json::to_value(&selected)?;
        let mut tx = self.pool.begin().await?;
        // The project fence also serializes first clicks, before any receipt
        // exists to lock, and keeps the same lock order as stop/revocation.
        training::lock_training_project(&mut tx, actor.scope()).await?;
        // Serialize a repeat click against this request, including after acceptance.
        let previous = sqlx::query("SELECT * FROM workflow_training_decision_receipts WHERE request_id=$1 AND tenant=$2 AND project=$3 FOR UPDATE")
            .bind(request_ref).bind(&actor.scope().tenant).bind(&actor.scope().project).fetch_optional(&mut tx.executor()).await?;
        if let Some(previous) = previous {
            if previous.try_get::<String, _>("actor")? != actor.subject()
                || previous.try_get::<String, _>("credential")? != actor.credential()
                || previous.try_get::<Uuid, _>("idempotency_key")? != idempotency_key
                || previous.try_get::<i64, _>("expected_revision")? != revision(request_revision)?
                || previous.try_get::<serde_json::Value, _>("selection")? != selection_json
                || previous.try_get::<bool, _>("approved")? != approve
            {
                return Err(RepositoryError::ImmutableConflict);
            }
            return Ok(serde_json::from_value(previous.try_get("receipt")?)?);
        }
        let request = training::lock_candidate_decision_request(
            &mut tx,
            actor,
            request_ref,
            request_revision,
        )
        .await?;
        let training::TrainingRequestPurpose::CandidateDecision { selection } = request.purpose
        else {
            return Err(training::TrainingError::InvalidCommand.into());
        };
        if selected.iter().any(|reference| {
            !selection.iter().any(|item| {
                item.review_ref == reference.review_ref
                    && item.candidate_hash == reference.candidate_hash
                    && item.guidance_digest == reference.guidance_digest
            })
        }) {
            return Err(training::TrainingError::InvalidCommand.into());
        }
        let mut receipt = TrainingDecisionReceipt {
            request_ref,
            decisions: Vec::new(),
            publications: Vec::new(),
        };
        let mut approved_reviews: BTreeMap<Uuid, (String, Vec<String>)> = BTreeMap::new();
        for item in selected {
            let stored = sqlx::query("SELECT r.seal,r.seal_content_digest,p.candidate,coalesce(g.associations,'[]'::jsonb) AS guidance FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id LEFT JOIN workflow_learning_guidance_batches g ON g.review_id=r.id WHERE r.id=$1 AND r.tenant=$2 AND r.project=$3 AND p.candidate_hash=$4")
                .bind(item.review_ref).bind(&actor.scope().tenant).bind(&actor.scope().project).bind(&item.candidate_hash).fetch_optional(&mut tx.executor()).await?.ok_or(RepositoryError::Unavailable)?;
            let candidate: Candidate = serde_json::from_value(stored.try_get("candidate")?)?;
            let guidance: serde_json::Value = stored.try_get("guidance")?;
            let seal = hydrate_review(
                &mut tx.executor(),
                stored.try_get("seal")?,
                &stored.try_get::<String, _>("seal_content_digest")?,
            )
            .await?;
            training::validate_human_confirmation(&item.outcome_confirmation, &candidate, &seal)?;
            if frozen::proposal_hash(&candidate)? != item.candidate_hash
                || plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&guidance)?)
                    != item.guidance_digest
            {
                return Err(RepositoryError::ImmutableConflict);
            }
            let outcome = if !approve {
                CandidateDecisionOutcome::DecisionDeclined
            } else {
                sqlx::query("INSERT INTO workflow_training_candidate_decisions(request_id,review_id,candidate_hash,action,actor,credential) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind(request_ref).bind(item.review_ref).bind(&item.candidate_hash).bind(serde_json::to_value(&item.action)?)
                    .bind(actor.subject()).bind(actor.credential()).execute(&mut tx.executor()).await?;
                match &item.action {
                    CandidateDecisionAction::Approve => {
                        approved_reviews
                            .entry(item.review_ref)
                            .or_insert_with(|| (item.guidance_digest.clone(), Vec::new()))
                            .1
                            .push(item.candidate_hash.clone());
                        CandidateDecisionOutcome::ApprovedPendingPublication
                    }
                    CandidateDecisionAction::Reject { .. } => CandidateDecisionOutcome::Rejected,
                    CandidateDecisionAction::RequestRevision { .. } => {
                        CandidateDecisionOutcome::RevisionRequested
                    }
                }
            };
            receipt.decisions.push(TrainingCandidateDecisionReceipt {
                reference: TrainingCandidateRef {
                    review_ref: item.review_ref,
                    candidate_hash: item.candidate_hash,
                },
                action: item.action,
                outcome_confirmation: if approve {
                    item.outcome_confirmation
                } else {
                    training::HumanOutcomeConfirmation::NotProvided
                },
                outcome,
            });
        }
        training::complete_candidate_decision_request(&mut tx, request_ref).await?;
        if !approve {
            sqlx::query("UPDATE workflow_training_requests SET state='declined' WHERE id=$1")
                .bind(request_ref)
                .execute(&mut tx.executor())
                .await?;
        }
        sqlx::query("INSERT INTO workflow_training_decision_receipts(request_id,tenant,project,actor,credential,idempotency_key,expected_revision,selection,approved,receipt) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(request_ref).bind(&actor.scope().tenant).bind(&actor.scope().project).bind(actor.subject()).bind(actor.credential())
            .bind(idempotency_key).bind(revision(request_revision)?).bind(selection_json).bind(approve).bind(serde_json::to_value(&receipt)?).execute(&mut tx.executor()).await?;
        for (review, (digest, hashes)) in approved_reviews {
            sqlx::query("SAVEPOINT training_publication")
                .execute(&mut tx.executor())
                .await?;
            let publication = self
                .publish_frozen_authorized(
                    &mut tx,
                    actor.scope(),
                    review,
                    &hashes,
                    &digest,
                    (actor, Some(request_ref)),
                )
                .await;
            if publication.is_err() {
                sqlx::query("ROLLBACK TO SAVEPOINT training_publication")
                    .execute(&mut tx.executor())
                    .await?;
            }
            sqlx::query("RELEASE SAVEPOINT training_publication")
                .execute(&mut tx.executor())
                .await?;
            let outcome = match publication {
                Ok(publication) => {
                    receipt.publications.push(publication);
                    None
                }
                Err(RepositoryError::Training(_)) => Some(PublicationDeferral::Held),
                Err(RepositoryError::ImmutableConflict | RepositoryError::PortfolioConflict) => {
                    Some(PublicationDeferral::Conflict)
                }
                Err(
                    RepositoryError::IncompatibleTeachingFormat
                    | RepositoryError::ApprovalUnavailable,
                ) => Some(PublicationDeferral::Incompatible),
                Err(error) => return Err(error),
            };
            for decision in receipt.decisions.iter_mut().filter(|d| {
                d.reference.review_ref == review
                    && matches!(d.action, CandidateDecisionAction::Approve)
            }) {
                decision.outcome = if let Some(reason) = outcome {
                    CandidateDecisionOutcome::PublicationDeferred { reason }
                } else {
                    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_examples e JOIN workflow_learning_support s ON s.example_id=e.id JOIN workflow_learning_proposals p ON p.candidate->>'identity'=e.identity WHERE e.tenant=$1 AND e.project=$2 AND p.review_id=$3 AND s.review_id=p.review_id AND p.candidate_hash=$4 AND e.variant=p.candidate->>'variant' AND e.candidate#>'{state,evidence,program}'=p.candidate#>'{state,evidence,program}' AND e.state='active')")
                        .bind(&actor.scope().tenant).bind(&actor.scope().project).bind(decision.reference.review_ref).bind(&decision.reference.candidate_hash).fetch_one(&mut tx.executor()).await?;
                    if active {
                        CandidateDecisionOutcome::Published
                    } else {
                        CandidateDecisionOutcome::PublicationDeferred {
                            reason: PublicationDeferral::AdmissionDeclined,
                        }
                    }
                };
            }
        }
        sqlx::query(
            "UPDATE workflow_training_decision_receipts SET receipt=$2 WHERE request_id=$1",
        )
        .bind(request_ref)
        .bind(serde_json::to_value(&receipt)?)
        .execute(&mut tx.executor())
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
