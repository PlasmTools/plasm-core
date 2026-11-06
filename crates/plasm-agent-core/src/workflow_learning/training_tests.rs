use super::*;

fn scope() -> LibraryScope {
    LibraryScope {
        tenant: format!("training-test-{}", Uuid::new_v4()),
        project: "project".into(),
    }
}

#[test]
fn training_wire_cannot_supply_actor_or_authority() {
    let envelope = serde_json::json!({"idempotency_key":Uuid::new_v4(),"expected_revision":0,"command":{"action":"request"},"actor":"owner"});
    assert!(serde_json::from_value::<TrainingCommandEnvelope>(envelope).is_err());
    let envelope = serde_json::json!({"idempotency_key":Uuid::new_v4(),"expected_revision":0,"command":{"action":"request","grant":"forged"}});
    assert!(serde_json::from_value::<TrainingCommandEnvelope>(envelope).is_err());
}

#[test]
fn caller_identity_is_not_owner_authority() {
    let actor =
        TrustedTrainingActor::authenticated(scope(), "human".into(), "credential".into()).unwrap();
    assert!(!actor.is_human_owner());
    assert!(!actor.may_administer());
    assert!(
        TrustedTrainingActor::authenticated(scope(), String::new(), "credential".into()).is_err()
    );
}

async fn fixture() -> Option<(WorkflowRepository, LibraryScope, LogicalSessionId)> {
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").ok()?;
    let repo = WorkflowRepository::connect(&url).await.unwrap();
    repo.migrate().await.unwrap();
    let scope = scope();
    let logical = LogicalSessionId(Uuid::new_v4());
    repo.bind_session(&scope, logical).await.unwrap();
    Some((repo, scope, logical))
}

#[tokio::test]
async fn imported_review_attestation_requires_live_project_scoped_authority() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let host = TrustedTrainingActor::trusted_host(scope.clone(), "host".into(), "host-key".into())
        .unwrap();
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "human-key".into())
            .unwrap();
    let caller = TrustedTrainingActor::authenticated(
        scope.clone(),
        "delegate".into(),
        "delegate-key".into(),
    )
    .unwrap();
    let make_spec = |target, actions| TrainingGrantSpec {
        grantee: "delegate".into(),
        credential: "delegate-key".into(),
        actions,
        expires_at: Utc::now() + Duration::hours(1),
        publication_policy: PublicationPolicy::HumanReviewed,
        target,
    };
    assert!(matches!(
        repo.install_training_grant(
            &host,
            Uuid::new_v4(),
            make_spec(
                TrainingGrantTarget::Session {
                    logical_session: logical
                },
                vec![TrainingAction::AttestImportedReview]
            )
        )
        .await,
        Err(TrainingError::InvalidCommand)
    ));
    repo.install_training_grant(
        &host,
        Uuid::new_v4(),
        make_spec(TrainingGrantTarget::Project, vec![TrainingAction::Review]),
    )
    .await
    .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(matches!(
        authorize_import_attestation(&mut tx, &caller).await,
        Err(TrainingError::Denied)
    ));
    tx.rollback().await.unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let owner_permit = authorize_import_attestation(&mut tx, &owner).await.unwrap();
    assert!(matches!(owner_permit, ImportAttestationPermit::HumanOwner));
    recheck_import_attestation(&mut tx, &scope, &owner_permit)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let grant = repo
        .install_training_grant(
            &host,
            Uuid::new_v4(),
            make_spec(
                TrainingGrantTarget::Project,
                vec![TrainingAction::AttestImportedReview],
            ),
        )
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let permit = authorize_import_attestation(&mut tx, &caller)
        .await
        .unwrap();
    recheck_import_attestation(&mut tx, &scope, &permit)
        .await
        .unwrap();
    let foreign = LibraryScope {
        tenant: scope.tenant.clone(),
        project: "other".into(),
    };
    assert!(recheck_import_attestation(&mut tx, &foreign, &permit)
        .await
        .is_err());
    tx.commit().await.unwrap();

    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query("UPDATE workflow_training_grants SET revision=revision+1 WHERE id=$1")
        .bind(grant.id)
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(recheck_import_attestation(&mut tx, &scope, &permit)
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query("UPDATE workflow_training_grants SET revision=$2,expires_at=now()-interval '1 second' WHERE id=$1").bind(grant.id).bind(grant.revision as i64).execute(&mut connection).await.unwrap();
    drop(connection);
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(recheck_import_attestation(&mut tx, &scope, &permit)
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query("UPDATE workflow_training_grants SET expires_at=$2,revoked=true WHERE id=$1")
        .bind(grant.id)
        .bind(grant.expires_at)
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(recheck_import_attestation(&mut tx, &scope, &permit)
        .await
        .is_err());
    tx.rollback().await.unwrap();
}

fn command(revision: u64, command: TrainingCommand) -> TrainingCommandEnvelope {
    TrainingCommandEnvelope {
        idempotency_key: Uuid::new_v4(),
        expected_revision: revision,
        command,
    }
}

#[tokio::test]
async fn training_owner_authorizes_recorded_controller_prospectively() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let caller =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "credential".into())
            .unwrap();
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "browser".into()).unwrap();
    repo.bind_training_controller(&caller, logical)
        .await
        .unwrap();
    assert!(repo
        .training_command(
            &caller,
            logical,
            &command(0, TrainingCommand::AuthorizeController)
        )
        .await
        .is_err());
    let authorize = command(0, TrainingCommand::AuthorizeController);
    let first = repo
        .training_command(&owner, logical, &authorize)
        .await
        .unwrap();
    assert!(matches!(first.training_state, TrainingState::Active { .. }));
    assert_eq!(
        repo.training_command(&owner, logical, &authorize)
            .await
            .unwrap(),
        first
    );
    assert!(repo
        .authorize_training(&caller, logical, TrainingAction::Capture)
        .await
        .is_ok());
    let stopped = repo
        .training_command(
            &owner,
            logical,
            &command(first.revision, TrainingCommand::Stop),
        )
        .await
        .unwrap();
    let resumed = repo
        .training_command(
            &owner,
            logical,
            &command(stopped.revision, TrainingCommand::AuthorizeController),
        )
        .await
        .unwrap();
    assert!(resumed.held);
    assert!(matches!(
        resumed.training_state,
        TrainingState::Active { .. }
    ));
}

#[tokio::test]
async fn training_pending_human_consent_is_prospective_and_idempotent() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let caller =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "credential".into())
            .unwrap();
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "browser".into()).unwrap();
    assert_eq!(
        repo.training_status(&scope, logical)
            .await
            .unwrap()
            .training_state,
        TrainingState::Ordinary
    );
    let request = command(0, TrainingCommand::Request);
    repo.bind_training_controller(&caller, logical)
        .await
        .unwrap();
    let pending = repo
        .training_command(&caller, logical, &request)
        .await
        .unwrap();
    assert_eq!(
        repo.training_command(&caller, logical, &request)
            .await
            .unwrap(),
        pending
    );
    let TrainingState::AwaitingAuthorization { request_ref, .. } = pending.training_state else {
        panic!("consent must be pending")
    };
    let resolve = command(
        1,
        TrainingCommand::ResolveRequest {
            request_ref,
            request_revision: 1,
            approve: true,
        },
    );
    assert!(matches!(
        repo.training_command(&caller, logical, &resolve).await,
        Err(TrainingError::Denied)
    ));
    let active = repo
        .training_command(&owner, logical, &resolve)
        .await
        .unwrap();
    assert!(matches!(
        active.training_state,
        TrainingState::Active {
            start_revision: 2,
            ..
        }
    ));
    let mut tx = repo.pool.begin().await.unwrap();
    let permit = lock_training_epoch(&mut tx, &scope, logical).await.unwrap();
    tx.commit().await.unwrap();
    repo.training_command(&caller, logical, &command(2, TrainingCommand::Stop))
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(matches!(
        validate_training_epoch(&mut tx, &scope, logical, permit.epoch_id()).await,
        Err(TrainingError::Inactive)
    ));
    tx.rollback().await.unwrap();
    let resumed = repo
        .training_command(&caller, logical, &command(3, TrainingCommand::Resume))
        .await
        .unwrap();
    assert!(resumed.held);
    let mut tx = repo.pool.begin().await.unwrap();
    assert_ne!(
        lock_training_epoch(&mut tx, &scope, logical)
            .await
            .unwrap()
            .epoch_id(),
        permit.epoch_id()
    );
    assert!(matches!(
        lock_training_processing_epoch(&mut tx, &scope, logical, permit.epoch_id()).await,
        Err(TrainingError::Held)
    ));
}

#[tokio::test]
async fn training_grant_is_scoped_to_credential_and_revocation_fences_capture() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let host =
        TrustedTrainingActor::trusted_host(scope.clone(), "host".into(), "configuration".into())
            .unwrap();
    let spec = TrainingGrantSpec {
        grantee: "agent".into(),
        credential: "v1".into(),
        actions: vec![TrainingAction::Capture, TrainingAction::Review],
        expires_at: Utc::now() + Duration::hours(1),
        publication_policy: PublicationPolicy::HumanReviewed,
        target: TrainingGrantTarget::Session {
            logical_session: logical,
        },
    };
    let caller =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "v1".into()).unwrap();
    assert!(matches!(
        repo.install_training_grant(&caller, Uuid::new_v4(), spec.clone())
            .await,
        Err(TrainingError::Denied)
    ));
    let grant_key = Uuid::new_v4();
    let grant = repo
        .install_training_grant(&host, grant_key, spec.clone())
        .await
        .unwrap();
    assert_eq!(
        repo.install_training_grant(&host, grant_key, spec.clone())
            .await
            .unwrap()
            .id,
        grant.id
    );
    let mut changed = spec;
    changed.credential = "different".into();
    assert!(matches!(
        repo.install_training_grant(&host, grant_key, changed).await,
        Err(TrainingError::Conflict)
    ));
    repo.bind_training_controller(&caller, logical)
        .await
        .unwrap();
    let rotated =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "v2".into()).unwrap();
    assert!(matches!(
        repo.authorize_training(&rotated, logical, TrainingAction::Capture)
            .await,
        Err(TrainingError::Denied)
    ));
    repo.training_command(&caller, logical, &command(0, TrainingCommand::Request))
        .await
        .unwrap();
    let revoke_key = Uuid::new_v4();
    repo.revoke_training_grant(&host, revoke_key, grant.id, grant.revision)
        .await
        .unwrap();
    repo.revoke_training_grant(&host, revoke_key, grant.id, grant.revision)
        .await
        .unwrap();
    assert!(matches!(
        repo.revoke_training_grant(&host, revoke_key, grant.id, grant.revision + 1)
            .await,
        Err(TrainingError::Conflict)
    ));
    let state = repo.training_status(&scope, logical).await.unwrap();
    assert!(state.held);
    assert_eq!(
        state.training_state,
        TrainingState::Stopped {
            reason: TrainingStopReason::GrantUnavailable
        }
    );
}

#[tokio::test]
async fn training_concurrent_activation_has_one_epoch_and_conflicting_payload_is_rejected() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let owner =
        TrustedTrainingActor::trusted_host(scope.clone(), "host".into(), "host".into()).unwrap();
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    repo.install_training_grant(
        &owner,
        Uuid::new_v4(),
        TrainingGrantSpec {
            grantee: "agent".into(),
            credential: "key".into(),
            actions: vec![TrainingAction::Capture, TrainingAction::Review],
            expires_at: Utc::now() + Duration::hours(1),
            publication_policy: PublicationPolicy::HumanReviewed,
            target: TrainingGrantTarget::Session {
                logical_session: logical,
            },
        },
    )
    .await
    .unwrap();
    let a = command(0, TrainingCommand::Request);
    repo.bind_training_controller(&actor, logical)
        .await
        .unwrap();
    let b = command(0, TrainingCommand::Request);
    let (first, second) = tokio::join!(
        repo.training_command(&actor, logical, &a),
        repo.training_command(&actor, logical, &b)
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let mut conflicting = if first.is_ok() { a } else { b };
    conflicting.command = TrainingCommand::Stop;
    assert!(matches!(
        repo.training_command(&actor, logical, &conflicting).await,
        Err(TrainingError::Conflict)
    ));
    let mut connection = repo.pool.acquire().await.unwrap();
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM workflow_training_epochs WHERE tenant=$1 AND project=$2 AND logical_session=$3").bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).fetch_one(&mut connection).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn training_expired_grant_stops_capture_without_rewriting_prior_epoch() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let owner =
        TrustedTrainingActor::trusted_host(scope.clone(), "host".into(), "host".into()).unwrap();
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    let grant = repo
        .install_training_grant(
            &owner,
            Uuid::new_v4(),
            TrainingGrantSpec {
                grantee: "agent".into(),
                credential: "key".into(),
                actions: vec![TrainingAction::Capture, TrainingAction::Review],
                expires_at: Utc::now() + Duration::hours(1),
                publication_policy: PublicationPolicy::HumanReviewed,
                target: TrainingGrantTarget::Session {
                    logical_session: logical,
                },
            },
        )
        .await
        .unwrap();
    repo.bind_training_controller(&actor, logical)
        .await
        .unwrap();
    let active = repo
        .training_command(&actor, logical, &command(0, TrainingCommand::Request))
        .await
        .unwrap();
    let TrainingState::Active { epoch_ref, .. } = active.training_state else {
        panic!("active epoch required")
    };
    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query(
        "UPDATE workflow_training_grants SET expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(grant.id)
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);
    let stopped = repo.training_status(&scope, logical).await.unwrap();
    assert_eq!(
        stopped.training_state,
        TrainingState::Stopped {
            reason: TrainingStopReason::GrantUnavailable
        }
    );
    assert!(stopped.held);
    let pending = repo
        .training_command(
            &actor,
            logical,
            &command(stopped.revision, TrainingCommand::Resume),
        )
        .await
        .unwrap();
    assert!(matches!(
        pending.training_state,
        TrainingState::AwaitingAuthorization { .. }
    ));
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(matches!(
        lock_training_processing_epoch(&mut tx, &scope, logical, epoch_ref).await,
        Err(TrainingError::Held)
    ));
}

#[tokio::test]
async fn training_release_names_reviews_and_never_reopens_capture() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "browser".into()).unwrap();
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    repo.install_training_grant(
        &owner,
        Uuid::new_v4(),
        TrainingGrantSpec {
            grantee: "agent".into(),
            credential: "key".into(),
            actions: vec![TrainingAction::Capture, TrainingAction::Review],
            expires_at: Utc::now() + Duration::hours(1),
            publication_policy: PublicationPolicy::HumanReviewed,
            target: TrainingGrantTarget::Session {
                logical_session: logical,
            },
        },
    )
    .await
    .unwrap();
    repo.bind_training_controller(&actor, logical)
        .await
        .unwrap();
    let active = repo
        .training_command(&actor, logical, &command(0, TrainingCommand::Request))
        .await
        .unwrap();
    let TrainingState::Active { epoch_ref, .. } = active.training_state else {
        panic!("active epoch required")
    };
    let reviews = [Uuid::new_v4(), Uuid::new_v4()];
    let mut connection = repo.pool.acquire().await.unwrap();
    for (index, id) in reviews.iter().enumerate() {
        sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal,training_epoch) VALUES($1,$2,$3,$4,$5,'fixture','{}',$6)")
            .bind(id).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(index as i64).bind(epoch_ref).execute(&mut connection).await.unwrap();
    }
    drop(connection);
    repo.training_command(&actor, logical, &command(1, TrainingCommand::Stop))
        .await
        .unwrap();
    let release = command(
        2,
        TrainingCommand::ReleaseHeld {
            review_refs: vec![reviews[0]],
        },
    );
    assert!(matches!(
        repo.training_command(&actor, logical, &release).await,
        Err(TrainingError::Denied)
    ));
    repo.training_command(&owner, logical, &release)
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(matches!(
        lock_training_epoch(&mut tx, &scope, logical).await,
        Err(TrainingError::Inactive)
    ));
    assert!(
        lock_training_review(&mut tx, &scope, logical, reviews[0], epoch_ref)
            .await
            .is_ok()
    );
    assert!(matches!(
        lock_training_review(&mut tx, &scope, logical, reviews[1], epoch_ref).await,
        Err(TrainingError::Held)
    ));
}

#[tokio::test]
async fn training_controller_and_evaluation_boundaries_are_immutable() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    let stranger =
        TrustedTrainingActor::authenticated(scope.clone(), "other".into(), "key2".into()).unwrap();
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "browser".into()).unwrap();
    assert!(repo
        .training_command(&actor, logical, &command(0, TrainingCommand::Request))
        .await
        .is_err());
    repo.bind_training_session(&actor, logical, TrainingSessionPurpose::Evaluation)
        .await
        .unwrap();
    assert!(repo
        .bind_training_controller(&stranger, logical)
        .await
        .is_err());
    assert!(repo
        .bind_training_controller(&actor, logical)
        .await
        .is_err());
    for caller in [&actor, &owner, &stranger] {
        assert!(repo
            .training_command(caller, logical, &command(0, TrainingCommand::Request))
            .await
            .is_err());
        assert!(repo
            .training_command(
                caller,
                logical,
                &command(
                    0,
                    TrainingCommand::ResolveRequest {
                        request_ref: Uuid::new_v4(),
                        request_revision: 0,
                        approve: true
                    }
                )
            )
            .await
            .is_err());
    }
    let status = repo.training_status(&scope, logical).await.unwrap();
    assert!(repo
        .training_command(
            &owner,
            logical,
            &command(0, TrainingCommand::AuthorizeController)
        )
        .await
        .is_err());
    assert_eq!(status.session_purpose, TrainingSessionPurpose::Evaluation);
    assert_eq!(status.training_state, TrainingState::Ordinary);
}

#[tokio::test]
async fn training_named_run_release_requires_evidence_and_preserves_hold() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    let owner =
        TrustedTrainingActor::human_owner(scope.clone(), "owner".into(), "browser".into()).unwrap();
    repo.bind_training_controller(&actor, logical)
        .await
        .unwrap();
    repo.install_training_grant(
        &owner,
        Uuid::new_v4(),
        TrainingGrantSpec {
            grantee: "agent".into(),
            credential: "key".into(),
            actions: vec![
                TrainingAction::Capture,
                TrainingAction::Review,
                TrainingAction::Release,
            ],
            expires_at: Utc::now() + Duration::hours(1),
            publication_policy: PublicationPolicy::HumanReviewed,
            target: TrainingGrantTarget::Session {
                logical_session: logical,
            },
        },
    )
    .await
    .unwrap();
    let active = repo
        .training_command(&actor, logical, &command(0, TrainingCommand::Request))
        .await
        .unwrap();
    let TrainingState::Active { epoch_ref, .. } = active.training_state else {
        panic!("active required")
    };
    let runs = [
        ExecutionId::from_wire(&format!("pr{}", "a".repeat(64))).unwrap(),
        ExecutionId::from_wire(&format!("pr{}", "b".repeat(64))).unwrap(),
    ];
    let mut conn = repo.pool.acquire().await.unwrap();
    for run in runs {
        sqlx::query("INSERT INTO workflow_learning_receipts(tenant,project,logical_session,session_revision,execution_id,receipt,training_epoch) VALUES($1,$2,$3,1,$4,'{}',$5)")
            .bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(run.to_wire()).bind(epoch_ref).execute(&mut conn).await.unwrap();
    }
    drop(conn);
    repo.training_command(&actor, logical, &command(1, TrainingCommand::Stop))
        .await
        .unwrap();
    repo.training_command(
        &actor,
        logical,
        &command(
            2,
            TrainingCommand::ReleaseExecutions {
                execution_ids: vec![runs[0]],
            },
        ),
    )
    .await
    .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(
        authorize_review_executions(&mut tx, &scope, logical, &actor, 3, &runs)
            .await
            .is_err()
    );
    let permit = authorize_review_executions(&mut tx, &scope, logical, &actor, 3, &runs[..1])
        .await
        .unwrap();
    let review = Uuid::new_v4();
    sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal,training_epoch) VALUES($1,$2,$3,$4,1,'fixture','{}',$5)")
        .bind(review).bind(&scope.tenant).bind(&scope.project).bind(logical.to_string()).bind(epoch_ref).execute(&mut tx.executor()).await.unwrap();
    bind_released_review(&mut tx, &scope, logical, review, &permit)
        .await
        .unwrap();
    assert!(
        lock_training_review(&mut tx, &scope, logical, review, epoch_ref)
            .await
            .is_ok()
    );
    assert!(lock_training_epoch(&mut tx, &scope, logical).await.is_err());
    tx.commit().await.unwrap();
    repo.training_command(&actor, logical, &command(3, TrainingCommand::Stop))
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(
        authorize_review_executions(&mut tx, &scope, logical, &actor, 4, &runs[..1])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn training_issuer_revocation_fences_automatic_publication_and_is_idempotent() {
    let Some((repo, scope, logical)) = fixture().await else {
        return;
    };
    let owner = TrustedTrainingActor::human_owner(
        scope.clone(),
        "departing-owner".into(),
        "browser".into(),
    )
    .unwrap();
    let control =
        TrustedTrainingActor::trusted_host(scope.clone(), "control-plane".into(), "control".into())
            .unwrap();
    let actor =
        TrustedTrainingActor::authenticated(scope.clone(), "agent".into(), "key".into()).unwrap();
    repo.bind_training_controller(&actor, logical)
        .await
        .unwrap();
    let grant = repo
        .install_training_grant(
            &owner,
            Uuid::new_v4(),
            TrainingGrantSpec {
                grantee: "agent".into(),
                credential: "key".into(),
                actions: vec![
                    TrainingAction::Capture,
                    TrainingAction::Review,
                    TrainingAction::PublishAutomatically,
                ],
                expires_at: Utc::now() + Duration::hours(1),
                publication_policy: PublicationPolicy::DelegatedAutomatic,
                target: TrainingGrantTarget::Session {
                    logical_session: logical,
                },
            },
        )
        .await
        .unwrap();
    repo.training_command(&actor, logical, &command(0, TrainingCommand::Request))
        .await
        .unwrap();
    let mut capture_tx = repo.pool.begin().await.unwrap();
    let in_flight = lock_training_epoch(&mut capture_tx, &scope, logical)
        .await
        .unwrap();
    capture_tx.commit().await.unwrap();
    let key = Uuid::new_v4();
    let receipt = repo
        .revoke_training_issuer(&control, key, "departing-owner")
        .await
        .unwrap();
    assert_eq!(
        receipt.grants,
        vec![RevokedTrainingGrant {
            grant_ref: grant.id,
            revision: 2
        }]
    );
    assert_eq!(
        repo.revoke_training_issuer(&control, key, "departing-owner")
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        repo.revoke_training_issuer(&control, key, "another-owner")
            .await,
        Err(TrainingError::Conflict)
    ));
    let status = repo.training_status(&scope, logical).await.unwrap();
    assert!(status.held);
    assert!(matches!(
        status.training_state,
        TrainingState::Stopped { .. }
    ));
    let mut tx = repo.pool.begin().await.unwrap();
    assert!(authorize_publication(&mut tx, &scope, logical, &actor)
        .await
        .is_err());
    assert!(lock_training_epoch(&mut tx, &scope, logical).await.is_err());
    assert!(
        validate_training_epoch(&mut tx, &scope, logical, in_flight.epoch_id())
            .await
            .is_err()
    );
}
