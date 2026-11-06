//! Explicit release of already eligible run receipts, never historical backfill.
use super::*;

fn bounded(runs: &[ExecutionId]) -> Result<()> {
    if runs.is_empty()
        || runs.len() > 32
        || runs
            .iter()
            .enumerate()
            .any(|(i, id)| runs[..i].contains(id))
    {
        return Err(TrainingError::InvalidCommand);
    }
    Ok(())
}

async fn receipt_epoch(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    run: ExecutionId,
) -> Result<Uuid> {
    let epochs:Vec<Option<Uuid>>=sqlx::query_scalar("SELECT DISTINCT training_epoch FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND execution_id=$4")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).fetch_all(&mut tx.executor()).await?;
    match epochs.as_slice() {
        [Some(epoch)] => Ok(*epoch),
        [] | [None] => Err(TrainingError::Unavailable),
        _ => Err(TrainingError::Conflict),
    }
}

pub(super) async fn release_executions(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    runs: &[ExecutionId],
) -> Result<()> {
    bounded(runs)?;
    let mut epoch_refs = Vec::new();
    for run in runs {
        epoch_refs.push(receipt_epoch(tx, &actor.scope, logical, *run).await?);
    }
    if epoch_refs.iter().any(|epoch| *epoch != epoch_refs[0]) {
        return Err(TrainingError::InvalidCommand);
    }
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
    for run in runs {
        sqlx::query("INSERT INTO workflow_training_run_releases(tenant,project,logical_session,execution_id,epoch_id,grant_id,grant_revision,actor) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant,project,logical_session,execution_id) DO UPDATE SET epoch_id=EXCLUDED.epoch_id,grant_id=EXCLUDED.grant_id,grant_revision=EXCLUDED.grant_revision,actor=EXCLUDED.actor,revoked=false,released_at=now()")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(run.to_wire()).bind(epoch_refs[0]).bind(grant.id).bind(i64::try_from(grant.revision).map_err(|_|TrainingError::Conflict)?).bind(&actor.subject).execute(&mut tx.executor()).await?;
    }
    Ok(())
}

pub(super) async fn request_execution_release(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    id: Uuid,
    runs: &[ExecutionId],
) -> Result<()> {
    bounded(runs)?;
    let mut first = None;
    for run in runs {
        let epoch = receipt_epoch(tx, &actor.scope, logical, *run).await?;
        if first.is_some_and(|prior| prior != epoch) {
            return Err(TrainingError::InvalidCommand);
        }
        first = Some(epoch);
    }
    sqlx::query("INSERT INTO workflow_training_requests(id,tenant,project,logical_session,requester,credential,purpose,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',$8)")
        .bind(id).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(serde_json::to_value(TrainingRequestPurpose::ReleaseExecutions{execution_ids:runs.to_vec()})?).bind(Utc::now()+Duration::hours(24)).execute(&mut tx.executor()).await?;
    Ok(())
}

pub async fn authorize_review_executions(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    actor: &TrustedTrainingActor,
    expected_revision: u64,
    runs: &[ExecutionId],
) -> Result<TrainingEpochPermit> {
    bounded(runs)?;
    if actor.scope() != scope {
        return Err(TrainingError::Denied);
    }
    persistence::lock_training_project(tx, scope).await?;
    let row=sqlx::query("SELECT revision,controller,controller_credential,purpose FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3 FOR UPDATE")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Unavailable)?;
    if row.try_get::<String, _>("purpose")? == "evaluation" {
        return Err(TrainingError::Denied);
    }
    if row.try_get::<i64, _>("revision")?
        != i64::try_from(expected_revision).map_err(|_| TrainingError::Conflict)?
    {
        return Err(TrainingError::Conflict);
    }
    if !actor.may_administer()
        && (row.try_get::<String, _>("controller")? != actor.subject
            || row.try_get::<String, _>("controller_credential")? != actor.credential)
    {
        return Err(TrainingError::Denied);
    }
    match authorize_review(tx, scope, logical, actor, expected_revision).await {
        Ok(permit) => {
            let mut all_current = true;
            for run in runs {
                let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND execution_id=$4 AND training_epoch=$5)")
                    .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).bind(permit.epoch_id()).fetch_one(&mut tx.executor()).await?;
                all_current &= current;
            }
            if all_current {
                return Ok(permit);
            }
        }
        Err(
            TrainingError::Inactive
            | TrainingError::Held
            | TrainingError::GrantUnavailable
            | TrainingError::Denied,
        ) => {}
        Err(error) => return Err(error),
    }
    let mut released: Option<TrainingEpochPermit> = None;
    for run in runs {
        let row=sqlx::query("SELECT r.epoch_id,r.grant_revision,g.* FROM workflow_training_run_releases r JOIN workflow_training_grants g ON g.id=r.grant_id WHERE r.tenant=$1 AND r.project=$2 AND r.logical_session=$3 AND r.execution_id=$4 AND NOT r.revoked FOR UPDATE OF r,g")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Held)?;
        let grant = persistence::decode_grant(&row)?;
        if row.try_get::<bool, _>("revoked")?
            || grant.expires_at <= Utc::now()
            || row.try_get::<i64, _>("grant_revision")?
                != i64::try_from(grant.revision).map_err(|_| TrainingError::Conflict)?
            || !grant.actions.contains(&TrainingAction::Review)
        {
            return Err(TrainingError::GrantUnavailable);
        }
        let epoch = row.try_get("epoch_id")?;
        if let Some(prior) = &released {
            if prior.epoch_id() != epoch || prior.grant_id() != grant.id {
                return Err(TrainingError::Conflict);
            }
        }
        released = Some(TrainingEpochPermit {
            epoch_ref: epoch,
            grant_ref: grant.id,
            publication_policy: grant.publication_policy,
        });
    }
    released.ok_or(TrainingError::Held)
}

/// Persist the already authorized named-run release for the newly sealed review.
pub async fn bind_released_review(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    review_ref: Uuid,
    permit: &TrainingEpochPermit,
) -> Result<()> {
    let row=sqlx::query("SELECT grant_id,held FROM workflow_training_epochs WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4")
        .bind(permit.epoch_id()).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_one(&mut tx.executor()).await?;
    if row.try_get::<Uuid, _>("grant_id")? == permit.grant_id()
        && !row.try_get::<bool, _>("held")?
    {
        return Ok(());
    }
    sqlx::query("INSERT INTO workflow_training_releases(review_id,tenant,project,logical_session,epoch_id,grant_id,grant_revision,actor) SELECT $1,$2,$3,$4,$5,id,revision,issuer FROM workflow_training_grants WHERE id=$6 ON CONFLICT(review_id) DO NOTHING")
        .bind(review_ref).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(permit.epoch_id()).bind(permit.grant_id()).execute(&mut tx.executor()).await?;
    Ok(())
}

impl WorkflowRepository {
    pub async fn request_training_execution_release(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        expected_revision: u64,
        idempotency_key: Uuid,
        execution_ids: Vec<ExecutionId>,
    ) -> Result<TrainingDecisionRequest> {
        self.training_command(
            actor,
            logical,
            &TrainingCommandEnvelope {
                idempotency_key,
                expected_revision,
                command: TrainingCommand::RequestExecutionRelease { execution_ids },
            },
        )
        .await?;
        self.read_training_request(actor, idempotency_key).await
    }
}
