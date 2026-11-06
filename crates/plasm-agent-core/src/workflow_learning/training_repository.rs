use super::*;

pub async fn lock_training_project(tx: &mut Transaction<'_>, scope: &LibraryScope) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,709471033))")
        .bind(serde_json::to_string(scope)?)
        .execute(&mut tx.executor())
        .await?;
    Ok(())
}

fn revision(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| TrainingError::InvalidCommand)
}

async fn check_binding(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
) -> Result<()> {
    let bound: Option<String> = sqlx::query_scalar(
        "SELECT project FROM workflow_learning_sessions WHERE tenant=$1 AND logical_session=$2",
    )
    .bind(&scope.tenant)
    .bind(logical.to_string())
    .fetch_optional(&mut tx.executor())
    .await?;
    if bound.as_ref() != Some(&scope.project) {
        return Err(TrainingError::Unavailable);
    }
    Ok(())
}

pub(super) fn decode_grant(row: &sqlx::postgres::PgRow) -> Result<TrainingGrant> {
    let logical: Option<String> = row.try_get("logical_session")?;
    Ok(TrainingGrant {
        id: row.try_get("id")?,
        scope: LibraryScope {
            tenant: row.try_get("tenant")?,
            project: row.try_get("project")?,
        },
        issuer: row.try_get("issuer")?,
        grantee: row.try_get("grantee")?,
        credential: row.try_get("credential")?,
        actions: serde_json::from_value(row.try_get("permissions")?)?,
        publication_policy: serde_json::from_value(row.try_get("policy")?)?,
        revision: u64::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| TrainingError::Conflict)?,
        expires_at: row.try_get("expires_at")?,
        target: match logical {
            Some(id) => TrainingGrantTarget::Session {
                logical_session: LogicalSessionId(
                    Uuid::parse_str(&id).map_err(|_| TrainingError::Conflict)?,
                ),
            },
            None => TrainingGrantTarget::Project,
        },
    })
}

pub(super) async fn find_grant(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    logical: LogicalSessionId,
    action: TrainingAction,
) -> Result<Option<TrainingGrant>> {
    let rows = sqlx::query("SELECT * FROM workflow_training_grants WHERE tenant=$1 AND project=$2 AND grantee=$3 AND credential=$4 AND NOT revoked AND expires_at>now() AND (logical_session IS NULL OR logical_session=$5) ORDER BY created_at DESC,id LIMIT 128 FOR UPDATE")
        .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&actor.subject).bind(&actor.credential).bind(logical.to_string())
        .fetch_all(&mut tx.executor()).await?;
    for row in rows {
        let grant = decode_grant(&row)?;
        if grant.actions.contains(&action) {
            return Ok(Some(grant));
        }
    }
    Ok(None)
}

/// Imported source reviews have no destination session. Only an explicit
/// project-wide grant can authorize their external attestation.
pub(super) async fn find_project_grant(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    action: TrainingAction,
) -> Result<Option<TrainingGrant>> {
    let rows = sqlx::query("SELECT * FROM workflow_training_grants WHERE tenant=$1 AND project=$2 AND grantee=$3 AND credential=$4 AND logical_session IS NULL AND NOT revoked AND expires_at>now() ORDER BY created_at DESC,id LIMIT 128 FOR UPDATE")
        .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&actor.subject).bind(&actor.credential)
        .fetch_all(&mut tx.executor()).await?;
    for row in rows {
        let grant = decode_grant(&row)?;
        if grant.actions.contains(&action) {
            return Ok(Some(grant));
        }
    }
    Ok(None)
}

pub(super) async fn recheck_project_attestation(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    grant_id: Uuid,
    grant_revision: u64,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    let row = sqlx::query("SELECT * FROM workflow_training_grants WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session IS NULL AND NOT revoked AND expires_at>now() FOR UPDATE")
        .bind(grant_id).bind(&scope.tenant).bind(&scope.project).fetch_optional(&mut tx.executor()).await?
        .ok_or(TrainingError::Denied)?;
    let grant = decode_grant(&row)?;
    if grant.revision != grant_revision
        || grant.expires_at != expires_at
        || !grant
            .actions
            .contains(&TrainingAction::AttestImportedReview)
    {
        return Err(TrainingError::Denied);
    }
    Ok(())
}

pub(super) async fn insert_grant(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    spec: TrainingGrantSpec,
) -> Result<TrainingGrant> {
    if !actor.may_administer() {
        return Err(TrainingError::Denied);
    }
    if matches!(&spec.target, TrainingGrantTarget::Session { .. })
        && spec.actions.contains(&TrainingAction::AttestImportedReview)
    {
        return Err(TrainingError::InvalidCommand);
    }
    if spec.grantee.trim().is_empty()
        || spec.credential.trim().is_empty()
        || spec.actions.is_empty()
        || spec.expires_at <= Utc::now()
        || spec.actions.len() > 5
        || spec
            .actions
            .iter()
            .enumerate()
            .any(|(i, a)| spec.actions[..i].contains(a))
        || (spec.publication_policy == PublicationPolicy::DelegatedAutomatic
            && !spec.actions.contains(&TrainingAction::PublishAutomatically))
    {
        return Err(TrainingError::InvalidCommand);
    }
    let logical = match &spec.target {
        TrainingGrantTarget::Project => None,
        TrainingGrantTarget::Session { logical_session } => {
            check_binding(tx, &actor.scope, *logical_session).await?;
            Some(logical_session.to_string())
        }
    };
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO workflow_training_grants(id,tenant,project,issuer,grantee,credential,logical_session,permissions,policy,revision,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1,$10)")
        .bind(id).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&actor.subject).bind(&spec.grantee).bind(&spec.credential)
        .bind(logical).bind(serde_json::to_value(&spec.actions)?).bind(serde_json::to_value(&spec.publication_policy)?).bind(spec.expires_at)
        .execute(&mut tx.executor()).await?;
    sqlx::query("INSERT INTO workflow_training_grant_audit(grant_id,actor,credential,action,revision) VALUES($1,$2,$3,'issued',1)")
        .bind(id).bind(&actor.subject).bind(&actor.credential).execute(&mut tx.executor()).await?;
    Ok(TrainingGrant {
        id,
        scope: actor.scope.clone(),
        issuer: actor.subject.clone(),
        grantee: spec.grantee,
        credential: spec.credential,
        actions: spec.actions,
        publication_policy: spec.publication_policy,
        revision: 1,
        expires_at: spec.expires_at,
        target: spec.target,
    })
}

async fn epoch(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    epoch_id: Uuid,
    capture: bool,
) -> Result<TrainingEpochPermit> {
    let row=sqlx::query("SELECT e.closed,e.held,g.*,e.grant_revision FROM workflow_training_epochs e JOIN workflow_training_grants g ON g.id=e.grant_id WHERE e.id=$1 AND e.tenant=$2 AND e.project=$3 AND e.logical_session=$4 FOR UPDATE OF e,g")
        .bind(epoch_id).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Inactive)?;
    if capture && row.try_get::<bool, _>("closed")? {
        return Err(TrainingError::Inactive);
    }
    if !capture && row.try_get::<bool, _>("held")? {
        return Err(TrainingError::Held);
    }
    let grant = decode_grant(&row)?;
    if row.try_get::<bool, _>("revoked")?
        || grant.expires_at <= Utc::now()
        || row.try_get::<i64, _>("grant_revision")? != revision(grant.revision)?
    {
        return Err(TrainingError::GrantUnavailable);
    }
    if !grant.actions.contains(&if capture {
        TrainingAction::Capture
    } else {
        TrainingAction::Review
    }) {
        return Err(TrainingError::Denied);
    }
    Ok(TrainingEpochPermit {
        epoch_ref: epoch_id,
        grant_ref: grant.id,
        publication_policy: grant.publication_policy,
    })
}

/// Lock order: project fence, session, epoch, grant; all lifecycle writes share it.
pub async fn lock_training_epoch(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
) -> Result<TrainingEpochPermit> {
    lock_training_project(tx, scope).await?;
    let active:Option<Option<Uuid>>=sqlx::query_scalar("SELECT active_epoch FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3 FOR UPDATE")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?;
    epoch(
        tx,
        scope,
        logical,
        active.flatten().ok_or(TrainingError::Inactive)?,
        true,
    )
    .await
}

pub async fn validate_training_epoch(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    expected: Uuid,
) -> Result<()> {
    if lock_training_epoch(tx, scope, logical).await?.epoch_ref != expected {
        return Err(TrainingError::Inactive);
    }
    Ok(())
}

pub async fn lock_training_processing(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
) -> Result<TrainingEpochPermit> {
    let permit = lock_training_epoch(tx, scope, logical).await?;
    epoch(tx, scope, logical, permit.epoch_ref, false).await
}

pub async fn lock_training_processing_epoch(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    epoch_ref: Uuid,
) -> Result<TrainingEpochPermit> {
    lock_training_project(tx, scope).await?;
    epoch(tx, scope, logical, epoch_ref, false).await
}

pub async fn authorize_publication(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    actor: &TrustedTrainingActor,
) -> Result<TrainingEpochPermit> {
    if actor.scope != *scope {
        return Err(TrainingError::Denied);
    }
    let permit = lock_training_processing(tx, scope, logical).await?;
    if actor.is_human_owner() {
        return Ok(permit);
    }
    let grant = find_grant(tx, actor, logical, TrainingAction::PublishAutomatically)
        .await?
        .ok_or(TrainingError::Denied)?;
    if permit.publication_policy != PublicationPolicy::DelegatedAutomatic
        || grant.id != permit.grant_ref
    {
        return Err(TrainingError::Denied);
    }
    Ok(permit)
}

pub async fn authorize_review(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    actor: &TrustedTrainingActor,
    expected_revision: u64,
) -> Result<TrainingEpochPermit> {
    if actor.scope != *scope {
        return Err(TrainingError::Denied);
    }
    let permit = lock_training_processing(tx, scope, logical).await?;
    let current:i64=sqlx::query_scalar("SELECT revision FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_one(&mut tx.executor()).await?;
    if current != revision(expected_revision)? {
        return Err(TrainingError::Conflict);
    }
    if !actor.is_human_owner() {
        let grant = find_grant(tx, actor, logical, TrainingAction::Review)
            .await?
            .ok_or(TrainingError::Denied)?;
        if grant.id != permit.grant_ref {
            return Err(TrainingError::Denied);
        }
    }
    Ok(permit)
}

#[async_trait]
impl TrainingAuthority for WorkflowRepository {
    async fn authorize_training(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        action: TrainingAction,
    ) -> Result<TrainingAuthorization> {
        let mut tx = self.pool.begin().await?;
        lock_training_project(&mut tx, &actor.scope).await?;
        check_binding(&mut tx, &actor.scope, logical).await?;
        let grant = find_grant(&mut tx, actor, logical, action)
            .await?
            .ok_or(TrainingError::Denied)?;
        tx.commit().await?;
        Ok(TrainingAuthorization { grant })
    }
}

impl WorkflowRepository {
    /// Called by authenticated session creation, never by a model lifecycle command.
    /// A known controller is immutable; existing unowned sessions stay ineligible.
    pub async fn bind_training_controller(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
    ) -> Result<()> {
        self.bind_training_session(actor, logical, TrainingSessionPurpose::Operational)
            .await
    }

    pub async fn bind_training_session(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        purpose: TrainingSessionPurpose,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        lock_training_project(&mut tx, &actor.scope).await?;
        check_binding(&mut tx, &actor.scope, logical).await?;
        let purpose = match purpose {
            TrainingSessionPurpose::Operational => "operational",
            TrainingSessionPurpose::Evaluation => "evaluation",
        };
        sqlx::query("INSERT INTO workflow_training_sessions(tenant,project,logical_session,controller,controller_credential,purpose) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(purpose).execute(&mut tx.executor()).await?;
        let row=sqlx::query("SELECT controller,controller_credential,purpose FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).fetch_one(&mut tx.executor()).await?;
        if row.try_get::<String, _>("controller")? != actor.subject
            || row.try_get::<String, _>("controller_credential")? != actor.credential
        {
            return Err(TrainingError::Denied);
        }
        if row.try_get::<String, _>("purpose")? != purpose {
            return Err(TrainingError::Denied);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn check_training_controller(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        check_binding(&mut tx, &actor.scope, logical).await?;
        let row=sqlx::query("SELECT controller,controller_credential FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Unavailable)?;
        if !actor.may_administer()
            && (row.try_get::<String, _>("controller")? != actor.subject
                || row.try_get::<String, _>("controller_credential")? != actor.credential)
        {
            return Err(TrainingError::Unavailable);
        }
        Ok(())
    }

    pub async fn training_status(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<TrainingStatus> {
        let mut tx = self.pool.begin().await?;
        lock_training_project(&mut tx, scope).await?;
        check_binding(&mut tx, scope, logical).await?;
        expire_session(&mut tx, scope, logical).await?;
        let row=sqlx::query("SELECT revision,state,held,purpose FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?;
        let mut status = match row {
            Some(row) => status(&row, logical)?,
            None => TrainingStatus {
                logical_session: logical,
                revision: 0,
                training_state: TrainingState::Ordinary,
                capture_availability: CaptureAvailability::Available,
                held: false,
                session_purpose: TrainingSessionPurpose::Operational,
            },
        };
        if matches!(status.training_state, TrainingState::Active { .. })
            && lock_training_epoch(&mut tx, scope, logical).await.is_err()
        {
            status.capture_availability = CaptureAvailability::Unavailable {
                reason: CaptureUnavailableReason::GrantUnavailable,
            };
        }
        tx.commit().await?;
        Ok(status)
    }

    pub async fn training_command(
        &self,
        actor: &TrustedTrainingActor,
        logical: LogicalSessionId,
        envelope: &TrainingCommandEnvelope,
    ) -> Result<TrainingStatus> {
        let mut tx = self.pool.begin().await?;
        lock_training_project(&mut tx, &actor.scope).await?;
        check_binding(&mut tx, &actor.scope, logical).await?;
        expire_session(&mut tx, &actor.scope, logical).await?;
        let command = serde_json::to_value(envelope)?;
        let stored=sqlx::query("SELECT command,receipt FROM workflow_training_commands WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND actor=$4 AND credential=$5 AND idempotency_key=$6")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(envelope.idempotency_key).fetch_optional(&mut tx.executor()).await?;
        if let Some(row) = stored {
            if row.try_get::<serde_json::Value, _>("command")? != command {
                return Err(TrainingError::Conflict);
            }
            return Ok(serde_json::from_value(row.try_get("receipt")?)?);
        }
        let row=sqlx::query("SELECT * FROM workflow_training_sessions WHERE tenant=$1 AND project=$2 AND logical_session=$3 FOR UPDATE")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Unavailable)?;
        let mut current = status(&row, logical)?;
        if current.session_purpose == TrainingSessionPurpose::Evaluation {
            return Err(TrainingError::Denied);
        }
        if current.revision != envelope.expected_revision {
            return Err(TrainingError::Conflict);
        }
        if !actor.may_administer()
            && (row.try_get::<String, _>("controller")? != actor.subject
                || row.try_get::<String, _>("controller_credential")? != actor.credential)
        {
            return Err(TrainingError::Denied);
        }
        let next = current
            .revision
            .checked_add(1)
            .ok_or(TrainingError::Conflict)?;
        match &envelope.command {
            TrainingCommand::AuthorizeController => {
                if !actor.is_human_owner() {
                    return Err(TrainingError::Denied);
                }
                if matches!(
                    current.training_state,
                    TrainingState::Active { .. } | TrainingState::AwaitingAuthorization { .. }
                ) {
                    return Err(TrainingError::Conflict);
                }
                let grant = insert_grant(
                    &mut tx,
                    actor,
                    TrainingGrantSpec {
                        grantee: row.try_get("controller")?,
                        credential: row.try_get("controller_credential")?,
                        actions: vec![TrainingAction::Capture, TrainingAction::Review],
                        expires_at: Utc::now() + Duration::hours(24),
                        publication_policy: PublicationPolicy::HumanReviewed,
                        target: TrainingGrantTarget::Session {
                            logical_session: logical,
                        },
                    },
                )
                .await?;
                current.training_state =
                    activate(&mut tx, &actor.scope, logical, next, &grant).await?;
            }
            TrainingCommand::Request | TrainingCommand::Resume => {
                if matches!(
                    current.training_state,
                    TrainingState::Active { .. } | TrainingState::AwaitingAuthorization { .. }
                ) {
                    return Err(TrainingError::Conflict);
                }
                if let Some(grant) =
                    find_grant(&mut tx, actor, logical, TrainingAction::Capture).await?
                {
                    current.training_state =
                        activate(&mut tx, &actor.scope, logical, next, &grant).await?;
                } else {
                    let request = Uuid::new_v4();
                    let expiry = Utc::now() + Duration::minutes(15);
                    sqlx::query("INSERT INTO workflow_training_requests(id,tenant,project,logical_session,requester,credential,purpose,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,'{\"kind\":\"consent\"}','pending',$7)")
                        .bind(request).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(expiry).execute(&mut tx.executor()).await?;
                    current.training_state = TrainingState::AwaitingAuthorization {
                        request_ref: request,
                        expires_at: expiry,
                    };
                }
            }
            TrainingCommand::Stop => {
                hold(&mut tx, &actor.scope, logical).await?;
                current.held = true;
                current.training_state = TrainingState::Stopped {
                    reason: TrainingStopReason::Requested,
                };
            }
            TrainingCommand::ResolveRequest {
                request_ref,
                request_revision,
                approve,
            } => {
                if !actor.is_human_owner() {
                    return Err(TrainingError::Denied);
                }
                if !matches!(current.training_state,TrainingState::AwaitingAuthorization{request_ref:r,..} if r==*request_ref)
                {
                    return Err(TrainingError::Conflict);
                }
                let request=sqlx::query("SELECT requester,credential FROM workflow_training_requests WHERE id=$1 AND tenant=$2 AND project=$3 AND logical_session=$4 AND revision=$5 AND state='pending' AND expires_at>now() FOR UPDATE")
                    .bind(request_ref).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(revision(*request_revision)?).fetch_optional(&mut tx.executor()).await?.ok_or(TrainingError::Unavailable)?;
                if *approve {
                    let grant = insert_grant(
                        &mut tx,
                        actor,
                        TrainingGrantSpec {
                            grantee: request.try_get("requester")?,
                            credential: request.try_get("credential")?,
                            actions: vec![TrainingAction::Capture, TrainingAction::Review],
                            expires_at: Utc::now() + Duration::hours(24),
                            publication_policy: PublicationPolicy::HumanReviewed,
                            target: TrainingGrantTarget::Session {
                                logical_session: logical,
                            },
                        },
                    )
                    .await?;
                    current.training_state =
                        activate(&mut tx, &actor.scope, logical, next, &grant).await?;
                } else {
                    current.training_state = TrainingState::Stopped {
                        reason: TrainingStopReason::Declined,
                    };
                }
                sqlx::query("UPDATE workflow_training_requests SET state=$1,revision=revision+1 WHERE id=$2")
                    .bind(if *approve {"approved"} else {"declined"}).bind(request_ref).execute(&mut tx.executor()).await?;
            }
            TrainingCommand::RequestCandidateDecision { selection } => {
                requests::insert_candidate_request(
                    &mut tx,
                    actor,
                    logical,
                    envelope.idempotency_key,
                    selection,
                )
                .await?;
            }
            TrainingCommand::ReleaseHeld { review_refs } => {
                release::release_reviews(&mut tx, actor, logical, review_refs).await?;
            }
            TrainingCommand::RequestRelease { review_refs } => {
                release::request_release(
                    &mut tx,
                    actor,
                    logical,
                    envelope.idempotency_key,
                    review_refs,
                )
                .await?;
            }
            TrainingCommand::ResolveRelease {
                request_ref,
                request_revision,
                approve,
            } => {
                release::resolve_release(
                    &mut tx,
                    actor,
                    logical,
                    *request_ref,
                    *request_revision,
                    *approve,
                )
                .await?;
            }
            TrainingCommand::ReleaseExecutions { execution_ids } => {
                run_release::release_executions(&mut tx, actor, logical, execution_ids).await?
            }
            TrainingCommand::RequestExecutionRelease { execution_ids } => {
                run_release::request_execution_release(
                    &mut tx,
                    actor,
                    logical,
                    envelope.idempotency_key,
                    execution_ids,
                )
                .await?
            }
        }
        current.revision = next;
        let active = match current.training_state {
            TrainingState::Active { epoch_ref, .. } => Some(epoch_ref),
            _ => None,
        };
        sqlx::query("UPDATE workflow_training_sessions SET state=$1,revision=$2,active_epoch=$3,held=$4 WHERE tenant=$5 AND project=$6 AND logical_session=$7")
            .bind(serde_json::to_value(&current.training_state)?).bind(revision(next)?).bind(active).bind(current.held).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
        let receipt = serde_json::to_value(&current)?;
        sqlx::query("INSERT INTO workflow_training_commands(tenant,project,logical_session,actor,credential,idempotency_key,command,receipt) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(envelope.idempotency_key).bind(&command).bind(&receipt).execute(&mut tx.executor()).await?;
        sqlx::query("INSERT INTO workflow_training_audit(tenant,project,logical_session,actor,credential,action,receipt) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(logical.to_string()).bind(&actor.subject).bind(&actor.credential).bind(command).bind(receipt).execute(&mut tx.executor()).await?;
        tx.commit().await?;
        Ok(current)
    }
}

fn status(row: &sqlx::postgres::PgRow, logical: LogicalSessionId) -> Result<TrainingStatus> {
    let purpose = match row.try_get::<String, _>("purpose")?.as_str() {
        "operational" => TrainingSessionPurpose::Operational,
        "evaluation" => TrainingSessionPurpose::Evaluation,
        _ => return Err(TrainingError::Conflict),
    };
    Ok(TrainingStatus {
        logical_session: logical,
        revision: u64::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| TrainingError::Conflict)?,
        training_state: serde_json::from_value(row.try_get("state")?)?,
        capture_availability: if purpose == TrainingSessionPurpose::Evaluation {
            CaptureAvailability::Unavailable {
                reason: CaptureUnavailableReason::EvaluationSession,
            }
        } else {
            CaptureAvailability::Available
        },
        held: row.try_get("held")?,
        session_purpose: purpose,
    })
}

async fn expire_session(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
) -> Result<()> {
    let expired:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_training_sessions s LEFT JOIN workflow_training_epochs e ON e.id=s.active_epoch LEFT JOIN workflow_training_grants g ON g.id=e.grant_id WHERE s.tenant=$1 AND s.project=$2 AND s.logical_session=$3 AND s.active_epoch IS NOT NULL AND (g.id IS NULL OR g.revoked OR g.expires_at<=now() OR g.revision<>e.grant_revision))")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_one(&mut tx.executor()).await?;
    if expired {
        hold(tx, scope, logical).await?;
        sqlx::query("UPDATE workflow_training_sessions SET active_epoch=NULL,held=true,revision=revision+1,state=$1 WHERE tenant=$2 AND project=$3 AND logical_session=$4")
            .bind(serde_json::to_value(TrainingState::Stopped{reason:TrainingStopReason::GrantUnavailable})?).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
    }
    Ok(())
}

async fn activate(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
    next: u64,
    grant: &TrainingGrant,
) -> Result<TrainingState> {
    let epoch = Uuid::new_v4();
    sqlx::query("INSERT INTO workflow_training_epochs(id,tenant,project,logical_session,grant_id,grant_revision,start_revision) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(epoch).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(grant.id).bind(revision(grant.revision)?).bind(revision(next)?).execute(&mut tx.executor()).await?;
    Ok(TrainingState::Active {
        epoch_ref: epoch,
        grant_ref: grant.id,
        start_revision: next,
        publication_policy: grant.publication_policy.clone(),
    })
}

async fn hold(
    tx: &mut Transaction<'_>,
    scope: &LibraryScope,
    logical: LogicalSessionId,
) -> Result<()> {
    sqlx::query("UPDATE workflow_training_run_releases SET revoked=true WHERE tenant=$1 AND project=$2 AND logical_session=$3")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
    sqlx::query("UPDATE workflow_training_releases SET revoked=true WHERE tenant=$1 AND project=$2 AND logical_session=$3")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
    sqlx::query("UPDATE workflow_training_epochs SET closed=true,held=true,closed_at=coalesce(closed_at,now()) WHERE tenant=$1 AND project=$2 AND logical_session=$3")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
    sqlx::query("UPDATE workflow_training_requests SET state='cancelled',revision=revision+1 WHERE tenant=$1 AND project=$2 AND logical_session=$3 AND state IN ('pending','approved')")
        .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).execute(&mut tx.executor()).await?;
    Ok(())
}

async fn revoke_grant(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    grant_ref: Uuid,
    expected_revision: u64,
) -> Result<()> {
    let changed=sqlx::query("UPDATE workflow_training_grants SET revoked=true,revision=revision+1 WHERE id=$1 AND tenant=$2 AND project=$3 AND revision=$4 AND NOT revoked")
            .bind(grant_ref).bind(&actor.scope.tenant).bind(&actor.scope.project).bind(revision(expected_revision)?).execute(&mut tx.executor()).await?;
    if changed.rows_affected() != 1 {
        return Err(TrainingError::Conflict);
    }
    sqlx::query("INSERT INTO workflow_training_grant_audit(grant_id,actor,credential,action,revision) VALUES($1,$2,$3,'revoked',$4)")
            .bind(grant_ref).bind(&actor.subject).bind(&actor.credential).bind(revision(expected_revision.checked_add(1).ok_or(TrainingError::Conflict)?)?).execute(&mut tx.executor()).await?;
    sqlx::query("UPDATE workflow_training_sessions SET active_epoch=NULL,held=true,state=$1,revision=revision+1 WHERE tenant=$2 AND project=$3 AND active_epoch IN (SELECT id FROM workflow_training_epochs WHERE grant_id=$4)")
            .bind(serde_json::to_value(TrainingState::Stopped { reason:TrainingStopReason::GrantUnavailable })?)
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(grant_ref).execute(&mut tx.executor()).await?;
    sqlx::query("UPDATE workflow_training_epochs SET closed=true,held=true,closed_at=coalesce(closed_at,now()) WHERE grant_id=$1")
            .bind(grant_ref).execute(&mut tx.executor()).await?;
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum GrantCommand<'a> {
    Install {
        spec: &'a TrainingGrantSpec,
    },
    Revoke {
        grant_ref: Uuid,
        expected_revision: u64,
    },
    RevokeIssuer {
        issuer_subject: &'a str,
    },
}

async fn grant_command_receipt(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    key: Uuid,
    command: &serde_json::Value,
) -> Result<Option<serde_json::Value>> {
    if !actor.may_administer() {
        return Err(TrainingError::Denied);
    }
    lock_training_project(tx, &actor.scope).await?;
    let row=sqlx::query("SELECT command,receipt FROM workflow_training_grant_commands WHERE tenant=$1 AND project=$2 AND actor=$3 AND credential=$4 AND idempotency_key=$5")
        .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&actor.subject).bind(&actor.credential).bind(key).fetch_optional(&mut tx.executor()).await?;
    row.map(|row| {
        if row.try_get::<serde_json::Value, _>("command")? != *command {
            return Err(TrainingError::Conflict);
        }
        Ok(row.try_get("receipt")?)
    })
    .transpose()
}

async fn save_grant_command(
    tx: &mut Transaction<'_>,
    actor: &TrustedTrainingActor,
    key: Uuid,
    command: serde_json::Value,
    receipt: serde_json::Value,
) -> Result<()> {
    sqlx::query("INSERT INTO workflow_training_grant_commands(tenant,project,actor,credential,idempotency_key,command,receipt) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(&actor.subject).bind(&actor.credential).bind(key).bind(command).bind(receipt).execute(&mut tx.executor()).await?;
    Ok(())
}

impl WorkflowRepository {
    pub async fn install_training_grant(
        &self,
        actor: &TrustedTrainingActor,
        idempotency_key: Uuid,
        spec: TrainingGrantSpec,
    ) -> Result<TrainingGrant> {
        let command = serde_json::to_value(GrantCommand::Install { spec: &spec })?;
        let mut tx = self.pool.begin().await?;
        if let Some(receipt) =
            grant_command_receipt(&mut tx, actor, idempotency_key, &command).await?
        {
            return Ok(serde_json::from_value(receipt)?);
        }
        let grant = insert_grant(&mut tx, actor, spec).await?;
        save_grant_command(
            &mut tx,
            actor,
            idempotency_key,
            command,
            serde_json::to_value(&grant)?,
        )
        .await?;
        tx.commit().await?;
        Ok(grant)
    }

    pub async fn revoke_training_grant(
        &self,
        actor: &TrustedTrainingActor,
        idempotency_key: Uuid,
        grant_ref: Uuid,
        expected_revision: u64,
    ) -> Result<()> {
        let command = serde_json::to_value(GrantCommand::Revoke {
            grant_ref,
            expected_revision,
        })?;
        let mut tx = self.pool.begin().await?;
        if grant_command_receipt(&mut tx, actor, idempotency_key, &command)
            .await?
            .is_some()
        {
            return Ok(());
        }
        revoke_grant(&mut tx, actor, grant_ref, expected_revision).await?;
        save_grant_command(
            &mut tx,
            actor,
            idempotency_key,
            command,
            serde_json::Value::Null,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

impl WorkflowRepository {
    /// Trusted control-plane projection; commit before acknowledging membership revocation.
    pub async fn revoke_training_issuer(
        &self,
        actor: &TrustedTrainingActor,
        idempotency_key: Uuid,
        issuer_subject: &str,
    ) -> Result<TrainingIssuerRevocation> {
        if issuer_subject.trim().is_empty() {
            return Err(TrainingError::InvalidCommand);
        }
        let command = serde_json::to_value(GrantCommand::RevokeIssuer { issuer_subject })?;
        let mut tx = self.pool.begin().await?;
        if let Some(receipt) =
            grant_command_receipt(&mut tx, actor, idempotency_key, &command).await?
        {
            return Ok(serde_json::from_value(receipt)?);
        }
        let rows=sqlx::query("SELECT id,revision FROM workflow_training_grants WHERE tenant=$1 AND project=$2 AND issuer=$3 AND NOT revoked ORDER BY id FOR UPDATE")
            .bind(&actor.scope.tenant).bind(&actor.scope.project).bind(issuer_subject).fetch_all(&mut tx.executor()).await?;
        let mut grants = Vec::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let revision = u64::try_from(row.try_get::<i64, _>("revision")?)
                .map_err(|_| TrainingError::Conflict)?;
            revoke_grant(&mut tx, actor, id, revision).await?;
            grants.push(RevokedTrainingGrant {
                grant_ref: id,
                revision: revision.checked_add(1).ok_or(TrainingError::Conflict)?,
            });
        }
        let receipt = TrainingIssuerRevocation {
            issuer_subject: issuer_subject.to_owned(),
            grants,
        };
        save_grant_command(
            &mut tx,
            actor,
            idempotency_key,
            command,
            serde_json::to_value(&receipt)?,
        )
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
