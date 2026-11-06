use super::*;

fn decode(row: &sqlx::postgres::PgRow) -> Result<TrainingDecisionRequest> {
    let state: String = row.try_get("state")?;
    Ok(TrainingDecisionRequest {
        request_ref: row.try_get("id")?,
        scope: LibraryScope {
            tenant: row.try_get("tenant")?,
            project: row.try_get("project")?,
        },
        logical_session: LogicalSessionId(
            Uuid::parse_str(&row.try_get::<String, _>("logical_session")?)
                .map_err(|_| TrainingError::Conflict)?,
        ),
        revision: u64::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| TrainingError::Conflict)?,
        requester: row.try_get("requester")?,
        purpose: serde_json::from_value(row.try_get("purpose")?)?,
        state: match state.as_str() {
            "pending" => TrainingRequestState::Pending,
            "approved" => TrainingRequestState::Approved,
            "declined" => TrainingRequestState::Declined,
            "cancelled" => TrainingRequestState::Cancelled,
            _ => return Err(TrainingError::Conflict),
        },
        expires_at: row.try_get("expires_at")?,
    })
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub(super) async fn insert_candidate_request(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    id: Uuid,
    selection: &[TrainingCandidateSelection],
) -> Result<()> {
    if selection.is_empty() || selection.len()>32 || selection.iter().enumerate().any(|(i,item)| {
        !digest(&item.candidate_hash) || !digest(&item.guidance_digest) || selection[..i].iter().any(|previous|previous.review_ref==item.review_ref && previous.candidate_hash==item.candidate_hash)
            || matches!(&item.action,CandidateDecisionAction::Reject{reason}|CandidateDecisionAction::RequestRevision{reason} if reason.trim().is_empty() || reason.len()>4096)
    }) {return Err(TrainingError::InvalidCommand);}
    for item in selection {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id WHERE r.id=$1 AND r.tenant=$2 AND r.project=$3 AND r.logical_session=$4 AND p.candidate_hash=$5)")
            .bind(item.review_ref).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&item.candidate_hash).fetch_one(&mut tx.executor()).await?;
        if !exists {
            return Err(TrainingError::Unavailable);
        }
        if !matches!(
            item.outcome_confirmation,
            HumanOutcomeConfirmation::NotProvided
        ) {
            let row=sqlx::query("SELECT r.seal,r.seal_content_digest,p.candidate FROM workflow_learning_reviews r JOIN workflow_learning_proposals p ON p.review_id=r.id WHERE r.id=$1 AND r.tenant=$2 AND r.project=$3 AND p.candidate_hash=$4")
                .bind(item.review_ref).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&item.candidate_hash).fetch_one(&mut tx.executor()).await?;
            let seal = super::super::hydrate_review(
                &mut tx.executor(),
                row.try_get("seal")?,
                &row.try_get::<String, _>("seal_content_digest")?,
            )
            .await
            .map_err(|_| TrainingError::Conflict)?;
            validate_human_confirmation(
                &item.outcome_confirmation,
                &serde_json::from_value(row.try_get("candidate")?)?,
                &seal,
            )?;
        }
    }
    let purpose = TrainingRequestPurpose::CandidateDecision {
        selection: selection.to_vec(),
    };
    sqlx::query("INSERT INTO workflow_training_requests(id,tenant,project,logical_session,requester,credential,purpose,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',$8)")
        .bind(id).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(serde_json::to_value(purpose)?).bind(Utc::now()+Duration::hours(24)).execute(&mut tx.executor()).await?;
    Ok(())
}

pub fn validate_human_confirmation(
    confirmation: &HumanOutcomeConfirmation,
    candidate: &Candidate,
    seal: &SealedReview,
) -> Result<()> {
    let HumanOutcomeConfirmation::Confirmed {
        execution_ids,
        statement,
    } = confirmation
    else {
        return Ok(());
    };
    if execution_ids.is_empty()
        || execution_ids.len() > 32
        || statement.trim().is_empty()
        || statement.len() > 4096
        || execution_ids
            .iter()
            .enumerate()
            .any(|(i, run)| execution_ids[..i].contains(run))
    {
        return Err(TrainingError::InvalidCommand);
    }
    let program = candidate
        .checked_program()
        .ok_or(TrainingError::InvalidCommand)?;
    for run in execution_ids {
        if !seal
            .request
            .subtasks
            .iter()
            .any(|task| task.execution_ids.contains(run))
            || !seal.executions.iter().any(|receipt| {
                receipt.execution_id == *run
                    && crate::workflow_learning::teaching::CheckedTeachingExample::from_observation(
                        &receipt.observation,
                    )
                    .is_ok_and(|observed| &observed == program)
            })
        {
            return Err(TrainingError::InvalidCommand);
        }
    }
    Ok(())
}

impl WorkflowRepository {
    /// Tenant authentication precedes this lookup; the caller must subsequently
    /// authorize the returned project before rendering any request details.
    pub async fn training_request_scope(
        &self,
        tenant: &str,
        request_ref: Uuid,
    ) -> Result<LibraryScope> {
        let mut connection = self.pool.acquire().await?;
        let project: Option<String> = sqlx::query_scalar(
            "SELECT project FROM workflow_training_requests WHERE id=$1 AND tenant=$2",
        )
        .bind(request_ref)
        .bind(tenant)
        .fetch_optional(&mut connection)
        .await?;
        Ok(LibraryScope {
            tenant: tenant.into(),
            project: project.ok_or(TrainingError::Unavailable)?,
        })
    }

    pub async fn read_training_request(
        &self,
        actor: &TrustedTrainingActor,
        request_ref: Uuid,
    ) -> Result<TrainingDecisionRequest> {
        let mut connection = self.pool.acquire().await?;
        let row = sqlx::query(
            "SELECT * FROM workflow_training_requests WHERE id=$1 AND tenant=$2 AND project=$3",
        )
        .bind(request_ref)
        .bind(&actor.scope.tenant)
        .bind(&actor.scope.project)
        .fetch_optional(&mut connection)
        .await?
        .ok_or(TrainingError::Unavailable)?;
        if !actor.may_administer()
            && (row.try_get::<String, _>("requester")? != actor.subject
                || row.try_get::<String, _>("credential")? != actor.credential)
        {
            return Err(TrainingError::Unavailable);
        }
        decode(&row)
    }

    pub async fn request_candidate_decision(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        expected_revision: u64,
        idempotency_key: Uuid,
        selection: Vec<TrainingCandidateSelection>,
    ) -> Result<TrainingDecisionRequest> {
        self.training_command(
            actor,
            logical,
            &TrainingCommandEnvelope {
                idempotency_key,
                expected_revision,
                command: TrainingCommand::RequestCandidateDecision { selection },
            },
        )
        .await?;
        self.read_training_request(actor, idempotency_key).await
    }
}

/// Caller completes publication/decision persistence in the same fenced transaction.
pub async fn lock_candidate_decision_request(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    request_ref: Uuid,
    expected_revision: u64,
) -> Result<TrainingDecisionRequest> {
    if !actor.is_human_owner() {
        return Err(TrainingError::Denied);
    }
    persistence::lock_training_project(tx, &actor.scope).await?;
    let row=sqlx::query("SELECT * FROM workflow_training_requests WHERE id=$1 AND tenant=$2 AND project=$3 FOR UPDATE")
        .bind(request_ref).bind(&actor.scope.tenant).bind(&actor.scope.project).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Unavailable)?;
    let request = decode(&row)?;
    if request.revision != expected_revision
        || request.state != TrainingRequestState::Pending
        || request.expires_at <= Utc::now()
    {
        return Err(TrainingError::Conflict);
    }
    if !matches!(
        request.purpose,
        TrainingRequestPurpose::CandidateDecision { .. }
    ) {
        return Err(TrainingError::InvalidCommand);
    }
    Ok(request)
}

pub async fn complete_candidate_decision_request(
    tx: &mut Transaction<'_>,
    request_ref: Uuid,
) -> Result<()> {
    let changed=sqlx::query("UPDATE workflow_training_requests SET state='approved',revision=revision+1 WHERE id=$1 AND state='pending'")
        .bind(request_ref).execute(&mut tx.executor()).await?;
    if changed.rows_affected() != 1 {
        return Err(TrainingError::Conflict);
    }
    Ok(())
}
