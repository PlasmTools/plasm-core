use super::*;
use crate::workflow_learning::test_identities as ids;

fn quality() -> Quality {
    Quality::unassessed(OutcomeAssurance::AgentAttested, 1)
}

async fn publish_test_candidate(
    repo: &WorkflowRepository,
    request: &ReviewRequest,
    revision: u64,
    mut candidate: Candidate,
    publish: bool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut request = request.clone();
    request.session_revision = revision;
    let handle = repo.review(&request).await?;
    let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
        .bind(handle.id).fetch_one(&mut repo.pool.acquire().await?).await?;
    let claim = ClaimedReview {
        id: handle.id,
        fence: 1,
        seal: serde_json::from_value(seal)?,
    };
    candidate.expected_family_version = repo
        .portfolio_version(&request.scope, &candidate.family, &candidate.variant)
        .await?;
    repo.finish(
        &claim,
        &[candidate],
        if publish {
            PublicationPolicy::Automatic
        } else {
            PublicationPolicy::Shadow
        },
    )
    .await?;
    Ok(())
}

#[test]
fn assurance_precedes_cost_and_readability_without_scalar_score() {
    let cheap = quality();
    let verified = Quality {
        assurance: OutcomeAssurance::PostconditionVerified,
        teaching_tokens: u32::MAX,
        ..quality()
    };
    assert!(verified.strictly_better_than(&cheap));
    assert!(!cheap.strictly_better_than(&verified));
    assert!(!cheap.strictly_better_than(&cheap));
}

#[test]
fn reviewed_publication_requires_separate_approval() {
    assert!(!PublicationPolicy::Shadow.permits_publication(false));
    assert!(!PublicationPolicy::Shadow.permits_publication(true));
    assert!(!PublicationPolicy::Reviewed.permits_publication(false));
    assert!(PublicationPolicy::Reviewed.permits_publication(true));
    assert!(PublicationPolicy::Automatic.permits_publication(false));
}

#[test]
fn reviews_require_outcomes_and_receipt_references() {
    let mut request = ReviewRequest {
        scope: LibraryScope {
            tenant: "t".into(),
            project: "p".into(),
        },
        logical_session: ids::logical("s"),
        session_revision: 1,
        intent: "work".into(),
        subtasks: vec![AchievedSubtask {
            intent: "work".into(),
            observed_outcome: "".into(),
            run_ids: vec![ids::run("r")],
        }],
    };
    assert!(validate_review(&request).is_err());
    request.subtasks[0].observed_outcome = "observed result".into();
    assert!(validate_review(&request).is_ok());
    request.subtasks[0].run_ids.clear();
    assert!(validate_review(&request).is_err());
}

#[test]
fn candidate_cannot_upgrade_assurance_or_inherit_unproved_rewrite() {
    let mut candidate = Candidate {
        intent: super::super::index::NormalizedIntent {
            purpose: "work".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "test-profile-v1".into(),
        family: "f".into(),
        variant: "v".into(),
        identity: "i".into(),
        coverage: vec!["subtask".into()],
        quality: quality(),
        catalog_fingerprints: [("catalog".into(), "hash".into())].into(),
        state: CandidateState::Proposed {
            diagnostic: ProposalDiagnostic {
                kind: DiagnosticKind::UnprovedRewrite,
                message: "unproved rewrite".into(),
            },
        },
        expected_family_version: 0,
    };
    assert!(!candidate.eligible(OutcomeAssurance::AgentAttested));
    candidate.state = CandidateState::Verified {
        evidence: VerifiedProgram::observed(
            super::super::teaching::tests::checked_fixture(),
            OutcomeAssurance::AgentAttested,
        ),
    };
    assert!(candidate.eligible(OutcomeAssurance::AgentAttested));
    candidate.quality.assurance = OutcomeAssurance::PostconditionVerified;
    assert!(!candidate.eligible(OutcomeAssurance::AgentAttested));
}

#[tokio::test]
async fn postgres_workflow_seals_and_fences() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let mut repo = WorkflowRepository::connect(&url).await?;
    repo.tenant_cap = 1000;
    repo.migrate().await?;
    let scope = LibraryScope {
        tenant: Uuid::new_v4().to_string(),
        project: "test".into(),
    };
    let session = LogicalSessionId::new_v4();
    repo.bind_session(&scope, session).await?;
    let mut receipt = ExecutionReceipt {
        scope: scope.clone(),
        logical_session: session,
        session_revision: 1,
        run_id: ids::run("run"),
        plan_id: ids::plan("plan"),
        prompt_hash: ids::prompt("prompt"),
        execute_session: ids::execute("execute"),
        run_artifact: serde_json::json!({"observed": true}),
        source: "class Read(Program): pass".into(),
        symbol_context: Default::default(),
        catalogue_requirements: Default::default(),
        compiled_plan: serde_json::json!({"typed": true}),
        observation: super::super::teaching::tests::checked_fixture().observation(),
        catalog_fingerprints: [("matrix".into(), "hash".into())].into(),
    };
    repo.record_execution(&receipt).await?;
    repo.record_execution(&receipt).await?;
    receipt.source.push_str(" changed");
    assert!(matches!(
        repo.record_execution(&receipt).await,
        Err(RepositoryError::ImmutableConflict)
    ));
    let request = ReviewRequest {
        scope: scope.clone(),
        logical_session: session,
        session_revision: 1,
        intent: "read".into(),
        subtasks: vec![AchievedSubtask {
            intent: "read".into(),
            observed_outcome: "row read".into(),
            run_ids: vec![ids::run("run")],
        }],
    };
    repo.begin_execution(&scope, request.logical_session, "attempt")
        .await?;
    assert!(matches!(
        repo.review(&request).await,
        Err(RepositoryError::InvalidEvidence(_))
    ));
    repo.finish_execution(&scope, request.logical_session, "attempt", true)
        .await?;
    let first = repo.review(&request).await?;
    assert_eq!(first.id, repo.review(&request).await?.id);
    let mut changed = request.clone();
    changed.intent = "different".into();
    assert!(matches!(
        repo.review(&changed).await,
        Err(RepositoryError::ImmutableConflict)
    ));
    let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
        .bind(first.id).fetch_one(&repo.pool).await?;
    let mut claim = ClaimedReview {
        id: first.id,
        fence: 0,
        seal: serde_json::from_value(seal)?,
    };
    assert!(matches!(
        repo.finish(&claim, &[], PublicationPolicy::Automatic).await,
        Err(RepositoryError::LeaseConflict)
    ));
    claim.fence = 1;
    let candidate = Candidate {
        intent: super::super::index::NormalizedIntent {
            purpose: "read".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "test-profile-v1".into(),
        family: "read".into(),
        variant: "matrix".into(),
        identity: "read-shape".into(),
        coverage: vec!["read".into()],
        quality: quality(),
        catalog_fingerprints: [("matrix".into(), "hash".into())].into(),
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(
                super::super::teaching::tests::checked_fixture(),
                OutcomeAssurance::AgentAttested,
            ),
        },
        expected_family_version: 0,
    };
    repo.finish(&claim, &[candidate], PublicationPolicy::Automatic)
        .await?;
    let active = repo.list_eligible(&scope, 10).await?;
    assert_eq!(1, active.len());
    let allowed = BTreeSet::from(["matrix".to_owned()]);
    let search = &active[0].candidate.intent;
    assert!(repo
        .search(
            &scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "different-model-same-dimensions",
                catalogs: &allowed,
                limit: 32,
                active_only: true
            }
        )
        .await?
        .is_empty());
    assert_eq!(
        1,
        repo.search(
            &scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &allowed,
                limit: 32,
                active_only: true
            }
        )
        .await?
        .len()
    );
    assert!(repo
        .search(
            &scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &BTreeSet::new(),
                limit: 32,
                active_only: true
            }
        )
        .await?
        .is_empty());
    let unrelated_scope = LibraryScope {
        tenant: "other-tenant".into(),
        project: scope.project.clone(),
    };
    assert!(repo
        .search(
            &unrelated_scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &allowed,
                limit: 32,
                active_only: true
            }
        )
        .await?
        .is_empty());
    let mut poorer = active[0].candidate.clone();
    poorer.identity = "inferior-same-coverage".into();
    poorer.quality.teaching_tokens = 1000;
    publish_test_candidate(&repo, &request, 10, poorer.clone(), true).await?;
    publish_test_candidate(&repo, &request, 11, poorer, true).await?;
    assert_eq!(1, repo.list_eligible(&scope, 10).await?.len());
    let inferior_count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND identity='inferior-same-coverage'")
        .bind(&scope.tenant).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(1, inferior_count);
    assert_eq!(
        2,
        repo.search(
            &scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &allowed,
                limit: 32,
                active_only: false
            }
        )
        .await?
        .len()
    );
    assert_eq!(
        1,
        repo.search(
            &scope,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &allowed,
                limit: 32,
                active_only: true
            }
        )
        .await?
        .len()
    );
    let mut changed_behavior = active[0].candidate.clone();
    changed_behavior.identity = "removed-guard".into();
    changed_behavior.state = CandidateState::Verified {
        evidence: VerifiedProgram::observed(
            super::super::teaching::tests::checked_predicate_fixture(),
            OutcomeAssurance::AgentAttested,
        ),
    };
    publish_test_candidate(&repo, &request, 16, changed_behavior, true).await?;
    let unchanged = repo.list_eligible(&scope, 10).await?;
    assert_eq!(1, unchanged.len());
    assert_eq!(active[0].id, unchanged[0].id);
    let guard = repo.lock_active_example(&scope, active[0].id).await?;
    let quarantine_repo = repo.clone();
    let quarantine_scope = scope.clone();
    let example_id = active[0].id;
    let mut quarantine = tokio::spawn(async move {
        quarantine_repo
            .quarantine(&quarantine_scope, example_id, "test drift")
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut quarantine)
            .await
            .is_err()
    );
    guard.commit().await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), quarantine).await???;
    assert!(matches!(
        repo.retrieve(&scope, example_id).await,
        Err(RepositoryError::Unavailable)
    ));
    assert!(matches!(
        repo.finish(&claim, &[], PublicationPolicy::Automatic).await,
        Err(RepositoryError::LeaseConflict)
    ));
    publish_test_candidate(&repo, &request, 12, active[0].candidate.clone(), true).await?;
    assert!(repo.list_eligible(&scope, 10).await?.is_empty());
    let mut shadow = active[0].candidate.clone();
    shadow.identity = "shadow".into();
    shadow.family = "shadow".into();
    publish_test_candidate(&repo, &request, 13, shadow.clone(), false).await?;
    publish_test_candidate(&repo, &request, 14, shadow.clone(), false).await?;
    assert!(repo.list_eligible(&scope, 10).await?.is_empty());
    publish_test_candidate(&repo, &request, 15, shadow, true).await?;
    let promoted = repo.list_eligible(&scope, 10).await?;
    assert_eq!(1, promoted.len());
    repo.quarantine(&scope, promoted[0].id, "test shadow drift")
        .await?;
    // Independent family writers must share the tenant cap lock.
    let payload = serde_json::to_value(&active[0].candidate)?;
    sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state) SELECT gen_random_uuid(),$1,'other-project','prefill','matrix',n::text,$2,'active' FROM generate_series(1,999) n")
        .bind(&scope.tenant).bind(payload).execute(&mut repo.pool.acquire().await?).await?;
    let mut races = Vec::new();
    for revision in [2, 3] {
        let mut next = request.clone();
        next.session_revision = revision;
        let handle = repo.review(&next).await?;
        let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
            .bind(handle.id).fetch_one(&mut repo.pool.acquire().await?).await?;
        let claimed = ClaimedReview {
            id: handle.id,
            fence: 1,
            seal: serde_json::from_value(seal)?,
        };
        let mut candidate = active[0].candidate.clone();
        candidate.family = format!("racing-family-{revision}");
        candidate.identity = candidate.family.clone();
        let publisher = repo.clone();
        races.push(tokio::spawn(async move {
            publisher
                .finish(&claimed, &[candidate], PublicationPolicy::Automatic)
                .await
        }));
    }
    for race in races {
        race.await??;
    }
    let active_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND state='active'",
    )
    .bind(&scope.tenant)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!(1000, active_count);
    let retained_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND state='candidate'",
    )
    .bind(&scope.tenant)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!(3, retained_count);
    let other_project = LibraryScope {
        tenant: scope.tenant.clone(),
        project: "other-project".into(),
    };
    assert_eq!(
        32,
        repo.search(
            &other_project,
            ExampleSearch {
                intent: search,
                embedding: &[1.0],
                profile: "test-profile-v1",
                catalogs: &allowed,
                limit: 1000,
                active_only: true
            }
        )
        .await?
        .len()
    );
    // Reviewed policy retains a candidate; only a scoped operator approval queues
    // fresh validation of the same immutable seal and permits publication.
    repo.tenant_cap = 1001;
    let mut approval_request = request.clone();
    approval_request.session_revision = 20;
    let approval_review = repo.review(&approval_request).await?;
    let approval_claim = repo.claim(60).await?.expect("approval test job");
    assert_eq!(approval_review.id, approval_claim.id);
    let mut approval_candidate = active[0].candidate.clone();
    approval_candidate.family = "operator-reviewed".into();
    approval_candidate.identity = "operator-reviewed".into();
    approval_candidate.expected_family_version = 0;
    repo.finish(
        &approval_claim,
        &[approval_candidate.clone()],
        PublicationPolicy::Reviewed,
    )
    .await?;
    let awaiting: String =
        sqlx::query_scalar("SELECT state FROM workflow_learning_reviews WHERE id=$1")
            .bind(approval_review.id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!("awaiting_approval", awaiting);
    assert!(repo.claim(60).await?.is_none());
    assert!(matches!(
        repo.approve_publication(&unrelated_scope, approval_review.id, "operator")
            .await,
        Err(RepositoryError::ApprovalUnavailable)
    ));
    repo.approve_publication(&scope, approval_review.id, "operator-123")
        .await?;
    repo.approve_publication(&scope, approval_review.id, "operator-456")
        .await?;
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM workflow_learning_publication_approvals WHERE review_id=$1",
    )
    .bind(approval_review.id)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!("operator-123", actor);
    let approved_claim = repo.claim(60).await?.expect("approved job");
    assert!(approved_claim.fence > approval_claim.fence);
    approval_candidate.expected_family_version = 1;
    repo.finish(
        &approved_claim,
        &[approval_candidate],
        PublicationPolicy::Reviewed,
    )
    .await?;
    let approved_active: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND family='operator-reviewed' AND state='active'")
        .bind(&scope.tenant).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(1, approved_active);

    // Retryable provider errors have durable due times and a three-failure cap.
    let mut retry_request = request.clone();
    retry_request.session_revision = 21;
    let retry_review = repo.review(&retry_request).await?;
    for failure in 1..=3 {
        let retry_claim = repo.claim(60).await?.expect("due retry job");
        assert_eq!(retry_review.id, retry_claim.id);
        repo.job_failure(&retry_claim, "provider temporarily unavailable", true)
            .await?;
        let row = sqlx::query("SELECT state,attempts,next_attempt>clock_timestamp() AS deferred,failure_reason FROM workflow_learning_reviews WHERE id=$1")
            .bind(retry_review.id).fetch_one(&mut repo.pool.acquire().await?).await?;
        assert_eq!(failure, row.try_get::<i32, _>("attempts")?);
        assert_eq!(
            if failure < 3 { "queued" } else { "failed" },
            row.try_get::<String, _>("state")?
        );
        assert!(row.try_get::<bool, _>("deferred")?);
        assert_eq!(
            "provider temporarily unavailable",
            row.try_get::<String, _>("failure_reason")?
        );
        assert!(repo.claim(60).await?.is_none());
        if failure < 3 {
            sqlx::query("UPDATE workflow_learning_reviews SET next_attempt=clock_timestamp()-interval '1 second' WHERE id=$1")
                .bind(retry_review.id).execute(&mut repo.pool.acquire().await?).await?;
        }
    }
    let mut expiry_request = request.clone();
    expiry_request.session_revision = 22;
    let expiry_review = repo.review(&expiry_request).await?;
    let expired_claim = repo.claim(60).await?.expect("expiry test job");
    assert_eq!(expiry_review.id, expired_claim.id);
    sqlx::query("UPDATE workflow_learning_reviews SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(expiry_review.id).execute(&mut repo.pool.acquire().await?).await?;
    assert!(repo.claim(60).await?.is_none());
    let expired_row = sqlx::query(
        "SELECT attempts,state,failure_reason FROM workflow_learning_reviews WHERE id=$1",
    )
    .bind(expiry_review.id)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!(1, expired_row.try_get::<i32, _>("attempts")?);
    assert_eq!("queued", expired_row.try_get::<String, _>("state")?);
    assert_eq!(
        "worker lease expired",
        expired_row.try_get::<String, _>("failure_reason")?
    );
    assert!(matches!(
        repo.job_failure(&expired_claim, "late old worker", true)
            .await,
        Err(RepositoryError::LeaseConflict)
    ));
    sqlx::query("UPDATE workflow_learning_reviews SET next_attempt=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(expiry_review.id).execute(&mut repo.pool.acquire().await?).await?;
    let reclaimed = repo.claim(60).await?.expect("due reclaimed job");
    assert!(reclaimed.fence > expired_claim.fence);
    repo.job_failure(&reclaimed, "permanent provider rejection", false)
        .await?;
    assert!(repo.claim(60).await?.is_none());

    // Preparing a seal must not expose a claimable job before the session gate commits.
    let mut pending_request = request.clone();
    pending_request.session_revision = 23;
    let pending = repo.prepare_review(&pending_request).await?;
    let uncommitted_id = pending.handle.id;
    let invisible: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_learning_reviews WHERE id=$1")
            .bind(uncommitted_id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!(0, invisible);
    assert!(repo.claim(60).await?.is_none());
    drop(pending);
    let handle = repo
        .prepare_review(&pending_request)
        .await?
        .commit()
        .await?;
    assert_ne!(uncommitted_id, handle.id);
    let complementary_claim = repo.claim(60).await?.expect("committed prepared job");
    assert_eq!(handle.id, complementary_claim.id);

    // One seal can retain complementary coverage in the same family, using one CAS.
    repo.tenant_cap = 1004;
    let complementary: Vec<_> = (0..4)
        .rev()
        .map(|i| {
            let mut candidate = active[0].candidate.clone();
            candidate.family = "complementary".into();
            candidate.identity = format!("complementary-{i}");
            candidate.coverage = vec![format!("coverage-{i}")];
            candidate.expected_family_version = 0;
            candidate
        })
        .collect();
    repo.finish(
        &complementary_claim,
        &complementary,
        PublicationPolicy::Automatic,
    )
    .await?;
    assert_eq!(
        1,
        repo.portfolio_version(&scope, "complementary", "matrix")
            .await?
    );
    let selected: Vec<String> = sqlx::query_scalar("SELECT identity FROM workflow_learning_examples WHERE tenant=$1 AND family='complementary' AND state='active' ORDER BY identity")
        .bind(&scope.tenant).fetch_all(&mut repo.pool.acquire().await?).await?;
    assert_eq!(
        vec!["complementary-0", "complementary-1", "complementary-2"],
        selected
    );
    let mut conflict_request = request.clone();
    conflict_request.session_revision = 24;
    repo.review(&conflict_request).await?;
    let conflict_claim = repo.claim(60).await?.expect("group conflict job");
    let mut inconsistent = complementary[..2].to_vec();
    inconsistent[0].expected_family_version = 2;
    inconsistent[1].expected_family_version = 1;
    assert!(matches!(
        repo.finish(&conflict_claim, &inconsistent, PublicationPolicy::Automatic)
            .await,
        Err(RepositoryError::PortfolioConflict)
    ));
    assert_eq!(
        1,
        repo.portfolio_version(&scope, "complementary", "matrix")
            .await?
    );
    repo.job_failure(&conflict_claim, "test terminal cleanup", false)
        .await?;
    repo.begin_execution(&scope, request.logical_session, "failed-attempt")
        .await?;
    repo.finish_execution(&scope, request.logical_session, "failed-attempt", false)
        .await?;
    assert!(matches!(
        repo.review(&request).await,
        Err(RepositoryError::InvalidEvidence(_))
    ));
    assert!(matches!(
        repo.finish_execution(&scope, request.logical_session, "failed-attempt", true)
            .await,
        Err(RepositoryError::ImmutableConflict)
    ));
    // Scope cleanup leaves unrelated integration-test rows untouched.
    let mut connection = repo.pool.acquire().await?;
    sqlx::query("DELETE FROM workflow_learning_publication_approvals WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_execution_journal WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_support WHERE review_id IN (SELECT id FROM workflow_learning_reviews WHERE tenant=$1)").bind(&scope.tenant).execute(&mut connection).await?;
    sqlx::query("DELETE FROM workflow_learning_examples WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_families WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_tenants WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_frozen_publications WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_proposals WHERE review_id IN (SELECT id FROM workflow_learning_reviews WHERE tenant=$1)")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_proposal_batches WHERE review_id IN (SELECT id FROM workflow_learning_reviews WHERE tenant=$1)")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_reviews WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_receipts WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_learning_sessions WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    Ok(())
}

#[test]
fn unassessed_quality_abstains_before_cost_comparison() {
    let original = quality();
    let mut cheaper = original.clone();
    cheaper.teaching_tokens = 0;
    assert!(!cheaper.strictly_better_than(&original));
    assert!(!original.strictly_better_than(&cheaper));
    cheaper.assurance = OutcomeAssurance::PostconditionVerified;
    assert!(cheaper.strictly_better_than(&original));
}

#[test]
fn review_format_requires_current_nominal_symbol_authority() {
    let current = serde_json::json!({"executions":[{
        "observation":{"state":"available","value":{"program":{
            "format":"whole_program_example",
            "schema_version":crate::workflow_learning::teaching::TEACHING_SCHEMA_VERSION
        }}},
        "catalogue_requirements":{"state":"unavailable","diagnostic":"fixture"}
    }]});
    validate_seal_format(&current).unwrap();
    for version in [
        serde_json::json!(1),
        serde_json::json!("2"),
        serde_json::Value::Null,
    ] {
        let mut stale = current.clone();
        stale["executions"][0]["observation"]["value"]["program"]["schema_version"] = version;
        assert!(matches!(
            validate_seal_format(&stale),
            Err(RepositoryError::IncompatibleTeachingFormat)
        ));
    }
}
