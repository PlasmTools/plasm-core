use super::*;

pub async fn authorize_review_publication(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    review_ref: Uuid,
    epoch_ref: Uuid,
    actor: &TrustedTrainingActor,
) -> Result<TrainingEpochPermit> {
    if actor.scope() != scope {
        return Err(TrainingError::Denied);
    }
    let permit = lock_training_review(tx, scope, logical, review_ref, epoch_ref).await?;
    if actor.is_human_owner() {
        return Ok(permit);
    }
    let grant = persistence::find_grant(tx, actor, logical, TrainingAction::PublishAutomatically)
        .await?
        .ok_or(TrainingError::Denied)?;
    if permit.publication_policy() != &PublicationPolicy::DelegatedAutomatic
        || grant.id != permit.grant_id()
    {
        return Err(TrainingError::Denied);
    }
    Ok(permit)
}

pub async fn lock_training_review(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    review_ref: Uuid,
    epoch_ref: Uuid,
) -> Result<TrainingEpochPermit> {
    match lock_training_processing_epoch(tx, scope, logical, epoch_ref).await {
        Ok(permit) => return Ok(permit),
        Err(TrainingError::Held | TrainingError::GrantUnavailable) => {}
        Err(error) => return Err(error),
    }
    let row=sqlx::query("SELECT g.*,r.grant_revision FROM workflow_training_releases r JOIN workflow_training_grants g ON g.id=r.grant_id WHERE r.review_id=$1 AND r.tenant=$2 AND r.project=$3 AND r.logical_session=$4 AND r.epoch_id=$5 AND NOT r.revoked FOR UPDATE OF r,g")
        .bind(review_ref).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(epoch_ref).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Held)?;
    let grant = persistence::decode_grant(&row)?;
    if row.try_get::<bool, _>("revoked")?
        || grant.expires_at <= Utc::now()
        || row.try_get::<i64, _>("grant_revision")? != grant.revision as i64
        || !grant.actions.contains(&TrainingAction::Review)
    {
        return Err(TrainingError::GrantUnavailable);
    }
    Ok(TrainingEpochPermit {
        epoch_ref,
        grant_ref: grant.id,
        publication_policy: grant.publication_policy,
    })
}

fn validate_refs(refs: &[Uuid]) -> Result<()> {
    if refs.is_empty()
        || refs.len() > 32
        || refs
            .iter()
            .enumerate()
            .any(|(i, id)| refs[..i].contains(id))
    {
        return Err(TrainingError::InvalidCommand);
    }
    Ok(())
}

async fn epochs(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    refs: &[Uuid],
) -> Result<Vec<(Uuid, Uuid)>> {
    validate_refs(refs)?;
    let mut result = Vec::new();
    for id in refs {
        let epoch:Option<Uuid>=sqlx::query_scalar("SELECT training_epoch FROM workflow_learning_reviews WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4")
            .bind(id).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?.flatten();
        result.push((*id, epoch.ok_or(TrainingError::Unavailable)?));
    }
    Ok(result)
}

pub(super) async fn release_reviews(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    refs: &[Uuid],
) -> Result<()> {
    let epochs = epochs(tx, &actor.scope, logical, refs).await?;
    let grant = if actor.may_administer() {
        persistence::insert_grant(
            tx,
            actor,
            TrainingGrantSpec {
                grantee: actor.subject.clone(),
                credential: actor.credential.clone(),
                actions: vec![TrainingAction::Review, TrainingAction::Release],
                expires_at: Utc::now() + Duration::hours(24),
                publication_policy: PublicationPolicy::HumanReviewed,
                target: TrainingGrantTarget::Session {
                    logical_session: logical,
                },
            },
        )
        .await?
    } else {
        persistence::find_grant(tx, actor, logical, TrainingAction::Release)
            .await?
            .ok_or(TrainingError::Denied)?
    };
    if !grant.actions.contains(&TrainingAction::Review) {
        return Err(TrainingError::Denied);
    }
    for (review, epoch) in epochs {
        sqlx::query("INSERT INTO workflow_training_releases(review_id,tenant,project,logical_session,epoch_id,grant_id,grant_revision,actor) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(review_id) DO UPDATE SET grant_id=EXCLUDED.grant_id,grant_revision=EXCLUDED.grant_revision,actor=EXCLUDED.actor,revoked=false,released_at=now()")
            .bind(review).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(epoch).bind(grant.id).bind(grant.revision as i64).bind(&actor.subject).execute(&mut tx.executor()).await?;
        sqlx::query("UPDATE workflow_training_requests SET state='cancelled',revision=revision+1 WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND state IN ('pending','approved') AND purpose->>'kind'='candidate_decision' AND EXISTS(SELECT 1 FROM jsonb_array_elements(purpose->'selection') item WHERE item->>'review_ref'=$4)")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(review.to_string()).execute(&mut tx.executor()).await?;
        // A failed/leased worker can be safely reclaimed only by its new fence.
        sqlx::query("UPDATE workflow_learning_reviews SET state='queued',fence=fence+1,lease_until=NULL,next_attempt=now(),failure_reason=NULL,attempts=0 WHERE id=$1 AND state IN ('failed','leased')")
            .bind(review).execute(&mut tx.executor()).await?;
    }
    Ok(())
}

pub(super) async fn request_release(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    id: Uuid,
    refs: &[Uuid],
) -> Result<()> {
    epochs(tx, &actor.scope, logical, refs).await?;
    sqlx::query("INSERT INTO workflow_training_requests(id,tenant,project,logical_session,requester,credential,purpose,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',$8)")
        .bind(id).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(serde_json::to_value(TrainingRequestPurpose::ReleaseHeld{review_refs:refs.to_vec()})?).bind(Utc::now()+Duration::hours(24)).execute(&mut tx.executor()).await?;
    Ok(())
}

pub(super) async fn resolve_release(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    id: Uuid,
    revision: u64,
    approve: bool,
) -> Result<()> {
    if !actor.is_human_owner() {
        return Err(TrainingError::Denied);
    }
    let row=sqlx::query("SELECT purpose FROM workflow_training_requests WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4 AND revision=$5 AND state='pending' AND expires_at>now() FOR UPDATE")
        .bind(id).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(i64::try_from(revision).map_err(|_|TrainingError::InvalidCommand)?).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Conflict)?;
    match serde_json::from_value(row.try_get("purpose")?)? {
        TrainingRequestPurpose::ReleaseHeld { review_refs } => {
            if approve {
                release_reviews(tx, actor, logical, &review_refs).await?;
            }
        }
        TrainingRequestPurpose::ReleaseExecutions { execution_ids } => {
            if approve {
                run_release::release_executions(tx, actor, logical, &execution_ids).await?;
            }
        }
        _ => return Err(TrainingError::InvalidCommand),
    }
    sqlx::query("UPDATE workflow_training_requests SET state=$1,revision=revision+1 WHERE id=$2")
        .bind(if approve { "approved" } else { "declined" })
        .bind(id)
        .execute(&mut tx.executor())
        .await?;
    Ok(())
}

impl WorkflowRepository {
    pub async fn request_training_release(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        expected_revision: u64,
        idempotency_key: Uuid,
        review_refs: Vec<Uuid>,
    ) -> Result<TrainingDecisionRequest> {
        self.training_command(
            actor,
            logical,
            &TrainingCommandEnvelope {
                idempotency_key,
                expected_revision,
                command: TrainingCommand::RequestRelease { review_refs },
            },
        )
        .await?;
        self.read_training_request(actor, idempotency_key).await
    }
}
