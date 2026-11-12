use super::*;
use crate::workflow_learning::test_identities as ids;

/// Optional read-only audit of a retained corpus; it makes no publication decisions.
#[tokio::test]
#[ignore = "requires explicit retained corpus and credentials"]
async fn retained_corpus_contract_recall_is_read_only(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("PLASM_RETAINED_CORPUS_URL")?;
    let scope = LibraryScope {
        tenant: std::env::var("PLASM_RETAINED_CORPUS_TENANT")?,
        project: std::env::var("PLASM_RETAINED_CORPUS_PROJECT")?,
    };
    let repo = WorkflowRepository::connect(&url).await?;
    let rows = repo.list_eligible(&scope, 1000).await?;
    let mut cross_family_hits = 0;
    let mut parameterized_pairs = 0;
    let mut identical_presentations = 0;
    for example in &rows {
        let found = repo
            .comparable_coverage(
                &scope,
                &example.candidate.catalog_fingerprints,
                &example.candidate.coverage,
                &example.candidate.intent,
                8,
            )
            .await?;
        if found
            .iter()
            .any(|other| other.candidate.family != example.candidate.family)
        {
            cross_family_hits += 1;
        }
        for other in &found {
            if other.id != example.id && example.id < other.id {
                if let (Some(a), Some(b)) = (
                    example.candidate.checked_program(),
                    other.candidate.checked_program(),
                ) {
                    if let Some(proof) = super::super::parameterization::prove(a, b) {
                        parameterized_pairs += 1;
                        identical_presentations += usize::from(proof.same_presentation);
                        println!(
                            "parameterized purposes: {:?} / {:?}",
                            example.candidate.intent.purpose, other.candidate.intent.purpose
                        );
                    }
                }
            }
        }
        assert!(found
            .iter()
            .all(|other| other.candidate.catalog_fingerprints
                == example.candidate.catalog_fingerprints
                && other.candidate.coverage == example.candidate.coverage));
    }
    println!(
        "audited {} active examples; {} nominate another family; {} have validated literal parameterization, {} with identical parameterized presentation",
        rows.len(),
        cross_family_hits,
        parameterized_pairs,
        identical_presentations
    );
    assert!(
        cross_family_hits > 0,
        "corpus has no observable cross-family comparison candidates"
    );
    assert!(
        parameterized_pairs > 0,
        "no retained pair satisfies the parameterization proof"
    );
    assert!(
        identical_presentations > 0,
        "no retained pair is eligible for same-family consolidation"
    );
    Ok(())
}

async fn fixture_seal(repo: &WorkflowRepository, value: serde_json::Value) -> Result<SealedReview> {
    let digest = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&value)?);
    hydrate_review(&mut repo.pool.acquire().await?, value, &digest).await
}

#[tokio::test]
async fn postgres_same_behavior_shorter_example_supersedes_atomically(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let repo = WorkflowRepository::connect(&url).await?;
    repo.migrate().await?;
    let session = super::super::teaching::tests::session(&["LangItem"]);
    let source = super::super::teaching::tests::source(&session);
    let verbose = format!("# A verbose explanation that consumes teaching tokens\n{source}");
    let filtered = source.replace(
        ".query().take(1)",
        ".query().where(lambda row: row.title == \"short\").select(\"title\").take(1)",
    );
    let filtered_long = format!(
        "# A verbose explanation that consumes teaching tokens\n{}",
        filtered.replace("short", "an exceptionally long user-selected title")
    );
    for sources in [
        vec![verbose.clone(), source.clone(), verbose],
        vec![
            filtered_long.clone(),
            filtered.clone(),
            filtered.replace("short", "another selected title"),
            filtered_long,
        ],
    ] {
        let scope = LibraryScope {
            tenant: Uuid::new_v4().to_string(),
            project: "portfolio-fixture".into(),
        };
        let mut old_id = None;
        let mut best_id = None;
        for (index, source) in sources.iter().enumerate() {
            let logical = LogicalSessionId::new_v4();
            repo.bind_training_fixture(&scope, logical).await?;
            let attempt = format!("attempt-{index}");
            repo.begin_execution(&scope, logical, &attempt).await?;
            let bundle = crate::plasm_dag::compile_python_program_checked(&session, source)?;
            let checked = super::super::teaching::CheckedTeachingExample::from_execution(
                &session, source, &bundle,
            )
            .await?;
            let execution_id = ids::run(&format!("portfolio-{index}"));
            let receipt = ExecutionReceipt {
                scope: scope.clone(),
                logical_session: logical,
                session_revision: 1,
                execution_id,
                plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
                prompt_hash: session.prompt_hash.parse()?,
                execute_session: ids::execute("portfolio"),
                source: source.clone(),
                compiled_plan: serde_json::to_value(&bundle.artifact().comp)?,
                teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
                    reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
                },
                observation: checked.observation(),
                catalogue_requirements:
                    super::super::catalogue_guidance::CatalogueRequirementEvidence::Unavailable {
                        diagnostic: "fixture".into(),
                    },
                catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
            };
            repo.record_training_fixture(&receipt).await?;
            repo.finish_execution(&scope, logical, &attempt, true)
                .await?;
            let request = ReviewRequest {
                scope: scope.clone(),
                logical_session: logical,
                session_revision: 1,
                intent: "inspect a fixture item".into(),
                subtasks: vec![AchievedSubtask {
                    intent: "inspect item".into(),
                    observed_outcome: "item observed".into(),
                    execution_ids: vec![execution_id],
                }],
            };
            let review = repo.review_training_fixture(&request).await?;
            let claim = repo
                .claim_scoped(60, Some(&scope))
                .await?
                .expect("review claim");
            assert_eq!(claim.id, review.id);
            let quality = Quality::observed(
                &claim.seal,
                &[execution_id],
                u32::try_from(plasm_core::o200k_token_count(source))?,
            )?;
            let candidate = Candidate {
                intent: super::super::index::NormalizedIntent {
                    purpose: "inspect item".into(),
                    facets: vec![],
                    aliases: vec![],
                },
                embedding: vec![1.0],
                embedding_profile: "fixture".into(),
                family: "fixture-family".into(),
                variant: "fixture-variant".into(),
                identity: checked.program().structural_fingerprint.clone(),
                coverage: vec!["read".into()],
                quality,
                catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
                state: CandidateState::Verified {
                    evidence: VerifiedProgram::observed(checked, OutcomeAssurance::AgentAttested),
                },
                expected_family_version: repo
                    .portfolio_version(&scope, "fixture-family", "fixture-variant")
                    .await?,
            };
            let candidate_identity = candidate.identity.clone();
            repo.finish(&claim, &[candidate], PublicationPolicy::Automatic)
                .await?;
            let active = repo.list_eligible(&scope, 10).await?;
            assert_eq!(active.len(), 1);
            if index == 0 {
                old_id = Some(active[0].id);
            } else if index == 1 {
                assert_ne!(old_id, Some(active[0].id));
                best_id = Some(active[0].id);
                let state: String =
                    sqlx::query_scalar("SELECT state FROM workflow_learning_examples WHERE id=$1")
                        .bind(old_id.expect("old revision"))
                        .fetch_one(&repo.pool)
                        .await?;
                assert_eq!(state, "superseded");
                assert_eq!(
                    active[0].candidate.program().expect("program").source,
                    *source
                );
            } else {
                assert_eq!(
                    best_id,
                    Some(active[0].id),
                    "weaker example must not displace the better revision"
                );
                assert_eq!(
                    active[0].candidate.program().expect("program").source,
                    sources[1]
                );
                if index == 2 && sources.len() == 4 {
                    let state: String = sqlx::query_scalar("SELECT state FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND identity=$3 ORDER BY created_at DESC LIMIT 1")
                        .bind(&scope.tenant).bind(&scope.project).bind(&candidate_identity)
                        .fetch_one(&repo.pool).await?;
                    assert_eq!(
                        state, "superseded",
                        "literal-only variants consolidate with their own evidence"
                    );
                }
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn unversioned_store_is_rejected_before_schema_changes(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let admin = sqlx::PgPool::connect(&url).await?;
    let schema = format!("workflow_unversioned_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await?;
    sqlx::query(&format!(
        "CREATE TABLE {schema}.workflow_learning_receipts(original text NOT NULL)"
    ))
    .execute(&admin)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO {schema}.workflow_learning_receipts VALUES ('preserved')"
    ))
    .execute(&admin)
    .await?;
    let separator = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let repo = WorkflowRepository::connect(&scoped).await?;
    assert!(matches!(
        repo.migrate().await,
        Err(RepositoryError::InvalidEvidence(
            "workflow learning store is unversioned; create a fresh store"
        ))
    ));
    let value: String = sqlx::query_scalar(&format!(
        "SELECT original FROM {schema}.workflow_learning_receipts"
    ))
    .fetch_one(&admin)
    .await?;
    assert_eq!(value, "preserved");
    let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(format!("{schema}.workflow_learning_schema_versions"))
        .fetch_one(&admin)
        .await?;
    assert!(
        exists.is_none(),
        "failed migration must roll back its version table"
    );
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await?;
    Ok(())
}

#[tokio::test]
async fn archive_preflight_schema_validation_is_read_only_and_fails_closed(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let suffix = Uuid::new_v4().simple().to_string();
    let schema = format!("workflow_preflight_{suffix}");
    let role = format!("workflow_reader_{suffix}");
    let admin = sqlx::PgPool::connect(&url).await?;
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await?;
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
        .execute(&admin)
        .await?;
    sqlx::query(&format!("GRANT USAGE ON SCHEMA {schema} TO {role}"))
        .execute(&admin)
        .await?;
    let separator = if url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{url}{separator}options=-csearch_path%3D{schema}%20-crole%3D{role}");
    let reader = WorkflowRepository::connect(&scoped_url).await?;
    assert!(
        reader.validate_current_schema().await.is_err(),
        "missing schema must fail without creating it"
    );
    sqlx::query(&format!(
        "CREATE TABLE {schema}.workflow_learning_schema_versions(version integer PRIMARY KEY)"
    ))
    .execute(&admin)
    .await?;
    sqlx::query(&format!(
        "GRANT SELECT ON {schema}.workflow_learning_schema_versions TO {role}"
    ))
    .execute(&admin)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO {schema}.workflow_learning_schema_versions(version) VALUES($1)"
    ))
    .bind(SCHEMA_VERSION - 1)
    .execute(&admin)
    .await?;
    assert!(
        reader.validate_current_schema().await.is_err(),
        "old schema must fail without upgrading"
    );
    sqlx::query(&format!(
        "INSERT INTO {schema}.workflow_learning_schema_versions(version) VALUES($1)"
    ))
    .bind(SCHEMA_VERSION)
    .execute(&admin)
    .await?;
    reader.validate_current_schema().await?;
    let mut connection = reader.pool.acquire().await?;
    let can_create: bool =
        sqlx::query_scalar("SELECT has_schema_privilege(current_user,$1,'CREATE')")
            .bind(&schema)
            .fetch_one(&mut connection)
            .await?;
    assert!(
        !can_create,
        "validation must succeed for a SELECT-only role"
    );
    drop(connection);
    drop(reader);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await?;
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&admin)
        .await?;
    Ok(())
}

#[tokio::test]
async fn current_schema_startup_does_not_lock_live_evidence_tables(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let repo = WorkflowRepository::connect(&url).await?;
    let (left, right) = tokio::join!(repo.migrate(), repo.migrate());
    left?;
    right?;
    let mut live = repo.pool.begin().await?;
    sqlx::query("LOCK TABLE workflow_learning_receipts IN ACCESS SHARE MODE")
        .execute(&mut live.executor())
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), repo.migrate()).await??;
    live.rollback().await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_learning_schema_versions WHERE version=(SELECT max(version) FROM workflow_learning_schema_versions)",
    )
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn training_capture_is_prospective_and_stop_fences_inflight_work(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let repo = WorkflowRepository::connect(&url).await?;
    repo.migrate().await?;
    let scope = LibraryScope {
        tenant: Uuid::new_v4().to_string(),
        project: "capture-boundaries".into(),
    };
    let logical = LogicalSessionId::new_v4();
    repo.bind_session(&scope, logical).await?;
    assert!(repo
        .begin_execution(&scope, logical, "ordinary")
        .await?
        .is_none());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_learning_execution_journal WHERE tenant=$1",
    )
    .bind(&scope.tenant)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert_eq!(count, 0);
    repo.bind_training_fixture(&scope, logical).await?;
    let permit = repo
        .begin_execution(&scope, logical, "authorized")
        .await?
        .expect("explicit training consent");
    let actor = training::TrustedTrainingActor::trusted_host(
        scope.clone(),
        "fixture-host".into(),
        "fixture-host-key".into(),
    )?;
    let status = repo.training_status(&scope, logical).await?;
    repo.training_command(
        &actor,
        logical,
        &training::TrainingCommandEnvelope {
            expected_revision: status.revision,
            idempotency_key: Uuid::new_v4(),
            command: training::TrainingCommand::Stop,
        },
    )
    .await?;
    let mut tx = repo.pool.begin().await?;
    assert!(
        training::validate_training_epoch(&mut tx, &scope, logical, permit.epoch_id())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert!(repo
        .begin_execution(&scope, logical, "stopped")
        .await?
        .is_none());
    let status = repo.training_status(&scope, logical).await?;
    repo.training_command(
        &actor,
        logical,
        &training::TrainingCommandEnvelope {
            expected_revision: status.revision,
            idempotency_key: Uuid::new_v4(),
            command: training::TrainingCommand::Resume,
        },
    )
    .await?;
    let resumed = repo
        .begin_execution(&scope, logical, "resumed")
        .await?
        .expect("resumed capture");
    assert_ne!(permit.epoch_id(), resumed.epoch_id());
    let mut tx = repo.pool.begin().await?;
    assert!(
        training::validate_training_epoch(&mut tx, &scope, logical, permit.epoch_id())
            .await
            .is_err()
    );
    tx.rollback().await?;
    Ok(())
}

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
    let handle = repo.review_training_fixture(&request).await?;
    let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
        .bind(handle.id).fetch_one(&mut repo.pool.acquire().await?).await?;
    let claim = ClaimedReview {
        id: handle.id,
        fence: 1,
        seal: fixture_seal(repo, seal).await?,
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
            execution_ids: vec![ids::run("r")],
        }],
    };
    assert!(validate_review(&request).is_err());
    request.subtasks[0].observed_outcome = "observed result".into();
    assert!(validate_review(&request).is_ok());
    request.subtasks[0].execution_ids.clear();
    assert!(validate_review(&request).is_err());
}

#[test]
fn direct_review_admission_rejects_unhydratable_reference_counts() {
    let subtask = AchievedSubtask {
        intent: "inspect".into(),
        observed_outcome: "completed".into(),
        execution_ids: vec![ids::run("execution")],
    };
    let mut request = ReviewRequest {
        scope: LibraryScope {
            tenant: "tenant".into(),
            project: "project".into(),
        },
        logical_session: ids::logical("session"),
        session_revision: 1,
        intent: "inspect".into(),
        subtasks: vec![subtask.clone(); 16],
    };
    assert!(validate_review(&request).is_ok());
    request.subtasks.push(subtask);
    assert!(matches!(
        validate_review(&request),
        Err(RepositoryError::InvalidEvidence(_))
    ));
    request.subtasks.truncate(1);
    request.subtasks[0].execution_ids = (0..32).map(|n| ids::run(&format!("{n}"))).collect();
    assert!(validate_review(&request).is_ok());
    request.subtasks[0].execution_ids.push(ids::run("extra"));
    assert!(matches!(
        validate_review(&request),
        Err(RepositoryError::InvalidEvidence(_))
    ));
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
    repo.bind_training_fixture(&scope, session).await?;
    let mut receipt = ExecutionReceipt {
        scope: scope.clone(),
        logical_session: session,
        session_revision: 1,
        execution_id: ids::run("run"),
        plan_id: ids::plan("plan"),
        prompt_hash: ids::prompt("prompt"),
        execute_session: ids::execute("execute"),
        source: "class Read(Program): pass".into(),
        teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
            reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },

        catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
        compiled_plan: serde_json::json!({"typed": true}),
        observation: super::super::teaching::tests::checked_fixture().observation(),
        catalog_fingerprints: [("matrix".into(), "hash".into())].into(),
    };
    repo.record_training_fixture(&receipt).await?;
    repo.record_training_fixture(&receipt).await?;
    receipt.source.push_str(" changed");
    assert!(matches!(
        repo.record_training_fixture(&receipt).await,
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
            execution_ids: vec![ids::run("run")],
        }],
    };
    repo.begin_execution(&scope, request.logical_session, "attempt")
        .await?;
    assert!(matches!(
        repo.review_training_fixture(&request).await,
        Err(RepositoryError::InvalidEvidence(_))
    ));
    repo.finish_execution(&scope, request.logical_session, "attempt", true)
        .await?;
    let first = repo.review_training_fixture(&request).await?;
    assert_eq!(first.id, repo.review_training_fixture(&request).await?.id);
    let mut changed = request.clone();
    changed.intent = "different".into();
    assert!(matches!(
        repo.review_training_fixture(&changed).await,
        Err(RepositoryError::ImmutableConflict)
    ));
    let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
        .bind(first.id).fetch_one(&repo.pool).await?;
    let mut claim = ClaimedReview {
        id: first.id,
        fence: 0,
        seal: fixture_seal(&repo, seal).await?,
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
    let contract_matches = repo
        .comparable_coverage(
            &scope,
            &active[0].candidate.catalog_fingerprints,
            &active[0].candidate.coverage,
            &active[0].candidate.intent,
            8,
        )
        .await?;
    assert_eq!(contract_matches.len(), 1);
    let different_contract = [("matrix".to_owned(), "different".to_owned())].into();
    assert!(repo
        .comparable_coverage(
            &scope,
            &different_contract,
            &active[0].candidate.coverage,
            &active[0].candidate.intent,
            8
        )
        .await?
        .is_empty());
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
        let handle = repo.review_training_fixture(&next).await?;
        let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
            .bind(handle.id).fetch_one(&mut repo.pool.acquire().await?).await?;
        let claimed = ClaimedReview {
            id: handle.id,
            fence: 1,
            seal: fixture_seal(&repo, seal).await?,
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
    let approval_review = repo.review_training_fixture(&approval_request).await?;
    let approval_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("approval test job");
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
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
    assert!(matches!(
        repo.approve_training_fixture(&unrelated_scope, approval_review.id, "operator")
            .await,
        Err(RepositoryError::InvalidEvidence(_))
    ));
    repo.approve_training_fixture(&scope, approval_review.id, "operator-123")
        .await?;
    repo.approve_training_fixture(&scope, approval_review.id, "operator-123")
        .await?;
    let actor: String =
        sqlx::query_scalar("SELECT actor FROM workflow_training_publications WHERE review_id=$1")
            .bind(approval_review.id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!("operator-123", actor);
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
    let approved_active: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND family='operator-reviewed' AND state='active'")
        .bind(&scope.tenant).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(1, approved_active);

    // Retryable provider errors have durable due times and a three-failure cap.
    let mut retry_request = request.clone();
    retry_request.session_revision = 21;
    let retry_review = repo.review_training_fixture(&retry_request).await?;
    for failure in 1..=3 {
        let retry_claim = repo
            .claim_scoped(60, Some(&scope))
            .await?
            .expect("due retry job");
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
        assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
        if failure < 3 {
            sqlx::query("UPDATE workflow_learning_reviews SET next_attempt=clock_timestamp()-interval '1 second' WHERE id=$1")
                .bind(retry_review.id).execute(&mut repo.pool.acquire().await?).await?;
        }
    }
    let mut expiry_request = request.clone();
    expiry_request.session_revision = 22;
    let expiry_review = repo.review_training_fixture(&expiry_request).await?;
    let expired_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("expiry test job");
    assert_eq!(expiry_review.id, expired_claim.id);
    sqlx::query("UPDATE workflow_learning_reviews SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(expiry_review.id).execute(&mut repo.pool.acquire().await?).await?;
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
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
    let reclaimed = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("due reclaimed job");
    assert!(reclaimed.fence > expired_claim.fence);
    repo.job_failure(&reclaimed, "permanent provider rejection", false)
        .await?;
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());

    // Preparing a seal must not expose a claimable job before the session gate commits.
    let mut pending_request = request.clone();
    pending_request.session_revision = 23;
    let pending = repo.prepare_training_fixture(&pending_request).await?;
    let uncommitted_id = pending.handle.id;
    let invisible: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_learning_reviews WHERE id=$1")
            .bind(uncommitted_id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!(0, invisible);
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
    drop(pending);
    let handle = repo
        .prepare_training_fixture(&pending_request)
        .await?
        .commit()
        .await?;
    assert_ne!(uncommitted_id, handle.id);
    let complementary_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("committed prepared job");
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
    // Identical verified programs from one review must not occupy separate
    // families merely because normalization assigned different names.
    repo.tenant_cap = 1100;
    let mut duplicate_request = request.clone();
    duplicate_request.session_revision = 25;
    repo.review_training_fixture(&duplicate_request).await?;
    let duplicate_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("duplicate job");
    let mut first = active[0].candidate.clone();
    first.family = "duplicate-a".into();
    first.identity = "cross-family-identity".into();
    first.intent.purpose = "cross-family duplicate fixture".into();
    first.expected_family_version = 0;
    let mut second = first.clone();
    second.family = "duplicate-b".into();
    repo.finish(
        &duplicate_claim,
        &[first, second],
        PublicationPolicy::Automatic,
    )
    .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND identity='cross-family-identity' AND state='active'")
        .bind(&scope.tenant).bind(&scope.project).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(count, 1);
    let support_count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_support s JOIN workflow_learning_examples e ON e.id=s.example_id WHERE s.review_id=$1 AND e.identity='cross-family-identity'")
        .bind(duplicate_claim.id).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(support_count, 1);
    let mut duplicate_races = Vec::new();
    for revision in [26, 27] {
        let mut next = request.clone();
        next.session_revision = revision;
        let handle = repo.review_training_fixture(&next).await?;
        let seal: serde_json::Value = sqlx::query_scalar("UPDATE workflow_learning_reviews SET state='leased',fence=1,lease_until=clock_timestamp()+interval '60 seconds' WHERE id=$1 RETURNING seal")
            .bind(handle.id).fetch_one(&mut repo.pool.acquire().await?).await?;
        let claim = ClaimedReview {
            id: handle.id,
            fence: 1,
            seal: fixture_seal(&repo, seal).await?,
        };
        let mut candidate = active[0].candidate.clone();
        candidate.family = format!("duplicate-race-{revision}");
        candidate.identity = "cross-family-race".into();
        candidate.intent.purpose = "concurrent duplicate fixture".into();
        candidate.expected_family_version = 0;
        let publisher = repo.clone();
        duplicate_races.push(tokio::spawn(async move {
            publisher
                .finish(&claim, &[candidate], PublicationPolicy::Automatic)
                .await
        }));
    }
    for race in duplicate_races {
        race.await??;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND identity='cross-family-race' AND state='active'")
        .bind(&scope.tenant).bind(&scope.project).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(count, 1);
    let supports: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_support s JOIN workflow_learning_examples e ON e.id=s.example_id WHERE e.tenant=$1 AND e.project=$2 AND e.identity='cross-family-race'")
        .bind(&scope.tenant).bind(&scope.project).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(supports, 2);
    let mut ambiguous_request = request.clone();
    ambiguous_request.session_revision = 28;
    repo.review_training_fixture(&ambiguous_request).await?;
    let ambiguous_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("ambiguous job");
    let mut ambiguous = active[0].candidate.clone();
    ambiguous.family = "similar-new-family".into();
    ambiguous.identity = "similar-different-program".into();
    ambiguous.intent.purpose = "concurrent duplicate fixture".into();
    ambiguous.expected_family_version = 0;
    repo.finish(&ambiguous_claim, &[ambiguous], PublicationPolicy::Automatic)
        .await?;
    let (state, reason): (String, Option<String>) = sqlx::query_as("SELECT state,reason FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND identity='similar-different-program'")
        .bind(&scope.tenant).bind(&scope.project).fetch_one(&mut repo.pool.acquire().await?).await?;
    assert_eq!(state, "candidate");
    assert_eq!(
        reason.as_deref(),
        Some("similar purpose and coverage across families require semantic review")
    );
    let mut conflict_request = request.clone();
    conflict_request.session_revision = 24;
    repo.review_training_fixture(&conflict_request).await?;
    let conflict_claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("group conflict job");
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
        repo.review_training_fixture(&request).await,
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
    sqlx::query("DELETE FROM workflow_training_review_commands WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_training_publications WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await?;
    sqlx::query("DELETE FROM workflow_training_automatic_decisions WHERE review_id IN (SELECT id FROM workflow_learning_reviews WHERE tenant=$1)").bind(&scope.tenant).execute(&mut connection).await?;
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
fn review_format_requires_typed_immutable_execution_and_program_references() {
    let current = serde_json::to_value(StoredReviewSeal {
        request: ReviewRequest {
            scope: LibraryScope {
                tenant: "tenant".into(),
                project: "project".into(),
            },
            logical_session: ids::logical("logical"),
            session_revision: 1,
            intent: "inspect".into(),
            subtasks: vec![],
        },
        executions: vec![ExecutionReceiptRef {
            session_revision: 1,
            execution_id: ids::run("run"),
            evidence_digest: ReceiptDigest::from_bytes(b"fact"),
            program_digest: ReceiptDigest::from_bytes(b"program"),
        }],
        assurance: OutcomeAssurance::AgentAttested,
    })
    .unwrap();
    validate_seal_format(&current).unwrap();
    for version in [
        serde_json::json!(1),
        serde_json::json!("not-a-digest"),
        serde_json::Value::Null,
    ] {
        let mut stale = current.clone();
        stale["executions"][0]["program_digest"] = version;
        assert!(matches!(
            validate_seal_format(&stale),
            Err(RepositoryError::IncompatibleTeachingFormat)
        ));
    }
}

// Exercises the persisted imported-support graph through two archive hops. This
// fixture inserts already-adopted records; authorization and publication are
// covered by the separate import tests.
#[tokio::test]
async fn imported_support_reexports_across_two_hops_without_review_claims(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    fn assert_no_result_or_grade(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, nested) in fields {
                    assert!(
                        !matches!(key.as_str(), "run_artifact" | "ledger_bytes" | "task_grade"),
                        "archive contains forbidden field {key}"
                    );
                    assert_no_result_or_grade(nested);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(assert_no_result_or_grade),
            _ => {}
        }
    }
    use super::super::{
        archive::ArchiveKind,
        filesystem_archive::{Reader, Record},
    };
    let Ok(url) = std::env::var("PLASM_ARCHIVE_GRAPH_TEST_URL") else {
        return Ok(());
    };
    let repo = WorkflowRepository::connect(&url).await?;
    repo.migrate().await?;
    let nonce = Uuid::new_v4();
    let source = LibraryScope {
        tenant: format!("graph-source-{nonce}"),
        project: "test".into(),
    };
    let session = LogicalSessionId::new_v4();
    repo.bind_training_fixture(&source, session).await?;
    let fixture_dir = std::path::PathBuf::from(crate::test_manifest_dir())
        .join("../../fixtures/schemas/plasm_language_matrix");
    let mut bound_cgs = plasm_core::loader::load_schema_dir(&fixture_dir)?;
    bound_cgs.bind_registry_entry_id("langmatrix");
    let bound_cgs = std::sync::Arc::new(bound_cgs);
    let mut session_fixture = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
        .entry_id("langmatrix")
        .entities(vec!["LangItem".to_owned()])
        .build(bound_cgs.clone());
    session_fixture.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &bound_cgs,
        "langmatrix",
        &["LangItem"],
    ));
    let source_text = super::super::teaching::tests::source(&session_fixture);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session_fixture, &source_text)?;
    let checked = super::super::teaching::CheckedTeachingExample::from_execution(
        &session_fixture,
        &source_text,
        &bundle,
    )
    .await?;
    let receipt = ExecutionReceipt {
        scope: source.clone(),
        logical_session: session,
        session_revision: 1,
        execution_id: ids::run("archive-graph-fixture"),
        plan_id: ids::plan("archive-graph-fixture"),
        prompt_hash: ids::prompt("archive-graph-fixture"),
        execute_session: ids::execute("archive-graph-fixture"),
        source: checked.program().source.clone(),
        compiled_plan: serde_json::json!({}),
        teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
            reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },
        observation: checked.observation(),
        catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
        catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
    };
    repo.record_training_fixture(&receipt).await?;
    let sibling_text = source_text.replace(
        ".query()",
        ".query().where(lambda row: row.id == row.title)",
    );
    let sibling_bundle =
        crate::plasm_dag::compile_python_program_checked(&session_fixture, &sibling_text)?;
    let sibling = super::super::teaching::CheckedTeachingExample::from_execution(
        &session_fixture,
        &sibling_text,
        &sibling_bundle,
    )
    .await?;
    let sibling_source = sibling.program().source.clone();
    let mut sibling_receipt = receipt.clone();
    sibling_receipt.execution_id = ids::run("archive-graph-sibling");
    sibling_receipt.plan_id = ids::plan("archive-graph-sibling");
    sibling_receipt.source = sibling_source.clone();
    sibling_receipt.observation = sibling.observation();
    sibling_receipt.catalog_fingerprints = sibling.program().catalog_fingerprints.clone();
    repo.record_training_fixture(&sibling_receipt).await?;
    let request = ReviewRequest {
        scope: source.clone(),
        logical_session: session,
        session_revision: 1,
        intent: "private sibling intent".into(),
        subtasks: vec![AchievedSubtask {
            intent: "private sibling claim".into(),
            observed_outcome: "private sibling outcome".into(),
            execution_ids: vec![receipt.execution_id, sibling_receipt.execution_id],
        }],
    };
    repo.begin_execution(&source, session, "archive-graph-fixture")
        .await?;
    repo.finish_execution(&source, session, "archive-graph-fixture", true)
        .await?;
    let candidate = Candidate {
        intent: super::super::index::NormalizedIntent {
            purpose: "read".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "fixture".into(),
        family: "read".into(),
        variant: "matrix".into(),
        identity: "shape".into(),
        coverage: vec!["read".into()],
        quality: quality(),
        catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(checked, OutcomeAssurance::AgentAttested),
        },
        expected_family_version: 0,
    };
    publish_test_candidate(&repo, &request, 1, candidate, true).await?;
    let scopes = [
        LibraryScope {
            tenant: format!("graph-b-{nonce}"),
            project: "test".into(),
        },
        LibraryScope {
            tenant: format!("graph-c-{nonce}"),
            project: "test".into(),
        },
    ];
    let root = std::env::var_os("PLASM_ARCHIVE_GRAPH_TEST_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("plasm-graph-test-{nonce}")));
    std::fs::create_dir(&root)?;
    let catalogue_dir = root.join("catalogues");
    std::fs::create_dir(&catalogue_dir)?;
    let cgs = bound_cgs.as_ref();
    let recipes = serde_json::to_vec(&plasm_compile::compile_cgs_capability_templates(cgs)?)?;
    let profile = plasm_core::catalog_discovery::EmbeddingProfile::default();
    let discovery = plasm_core::catalog_discovery::CatalogDiscoveryArtifact {
        entry_id: "langmatrix".into(),
        cgs_hash: cgs.catalog_cgs_hash_hex(),
        renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
        capabilities: plasm_core::catalog_discovery::capability_documents(cgs)?
            .into_iter()
            .enumerate()
            .map(|(index, document)| {
                let mut embedding = vec![0.0; profile.dimensions];
                embedding[index % profile.dimensions] = 1.0;
                plasm_core::catalog_discovery::EmbeddedCapability {
                    document,
                    embedding,
                }
            })
            .collect(),
        profile: profile.clone(),
        prerequisites: cgs.prerequisites.clone(),
    };
    discovery.validate(cgs)?;
    let discovery = serde_json::to_vec(&discovery)?;
    let manifest = plasm_core::catalog_il::CatalogManifest {
        format_version: 3,
        entry_id: "langmatrix".into(),
        version: cgs.version,
        cgs_hash: cgs.catalog_cgs_hash_hex(),
        label: "Graph fixture".into(),
        tags: vec![],
        cgs_json: "langmatrix.cgs.json".into(),
        recipes_json: "langmatrix.recipes.json".into(),
        recipes_hash: plasm_core::catalog_discovery::content_hash(&recipes),
        discovery_json: "langmatrix.discovery.json".into(),
        discovery_hash: plasm_core::catalog_discovery::content_hash(&discovery),
        embedding_profile: profile,
    };
    std::fs::write(
        catalogue_dir.join("langmatrix.cgs.json"),
        serde_json::to_vec(cgs)?,
    )?;
    std::fs::write(catalogue_dir.join("langmatrix.recipes.json"), recipes)?;
    std::fs::write(catalogue_dir.join("langmatrix.discovery.json"), discovery)?;
    std::fs::write(
        catalogue_dir.join("langmatrix.manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let source_path = root.join("source");
    repo.export_filesystem_archive(&source, &source_path, ArchiveKind::ServingLibrary)
        .await?;
    let mut current = source_path;
    for (hop, scope) in scopes.iter().enumerate() {
        let reader = Reader::open(&current)?;
        let (source_identity, source_candidate) = reader
            .manifest()
            .records
            .keys()
            .find_map(|identity| match reader.read_record(identity).ok()? {
                Record::Example {
                    revision_id,
                    candidate,
                    state: super::super::filesystem_archive::StoredExampleState::Active,
                    ..
                } => Some((format!("example:{revision_id}"), candidate)),
                _ => None,
            })
            .ok_or("no active example in fixture")?;
        let source_ref = reader
            .manifest()
            .records
            .get(&source_identity)
            .ok_or("missing item")?;
        let example_id = Uuid::new_v4();
        let mut tx = repo.pool.begin().await?;
        sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state) VALUES($1,$2,$3,$4,$5,$6,$7,'active')")
            .bind(example_id).bind(&scope.tenant).bind(&scope.project).bind(&source_candidate.family).bind(&source_candidate.variant).bind(&source_candidate.identity).bind(serde_json::to_value(&source_candidate)?).execute(&mut tx.executor()).await?;
        sqlx::query("INSERT INTO workflow_learning_archives(tenant,project,digest,document) VALUES($1,$2,$3,$4)")
            .bind(&scope.tenant).bind(&scope.project).bind(&reader.manifest().digest).bind(serde_json::to_value(reader.manifest())?).execute(&mut tx.executor()).await?;
        for (identity, reference) in &reader.manifest().records {
            let record = reader.read_record(identity)?;
            sqlx::query("INSERT INTO workflow_learning_archive_items(tenant,project,archive_digest,item_digest,identity,source_scope,record,disposition) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(&scope.tenant).bind(&scope.project).bind(&reader.manifest().digest).bind(&reference.digest).bind(identity).bind(serde_json::to_value(&reader.manifest().source_scope)?).bind(serde_json::to_value(record)?).bind(serde_json::json!({"state":"retained_observation"})).execute(&mut tx.executor()).await?;
        }
        sqlx::query("INSERT INTO workflow_learning_archive_example_support(example_id,tenant,project,archive_digest,item_digest) VALUES($1,$2,$3,$4,$5)")
            .bind(example_id).bind(&scope.tenant).bind(&scope.project).bind(&reader.manifest().digest).bind(&source_ref.digest).execute(&mut tx.executor()).await?;
        tx.commit().await?;
        let private_path = root.join(format!("private-hop-{hop}"));
        repo.export_filesystem_archive(scope, &private_path, ArchiveKind::PrivateCollection)
            .await?;
        let private = Reader::open(&private_path)?;
        for identity in private.manifest().records.keys() {
            let record = private.read_record(identity)?;
            assert_no_result_or_grade(&serde_json::to_value(&record)?);
        }
        let path = root.join(format!("hop-{hop}"));
        repo.export_filesystem_archive(scope, &path, ArchiveKind::ServingLibrary)
            .await?;
        let projected = Reader::open(&path)?;
        let mut count = 0;
        for identity in projected.manifest().records.keys() {
            let record = projected.read_record(identity)?;
            assert_no_result_or_grade(&serde_json::to_value(&record)?);
            let serialized = serde_json::to_string(&record)?;
            assert!(!serialized.contains("private sibling"));
            assert!(!serialized.contains(&serde_json::to_string(&sibling_source)?));
            match record {
                Record::SelectedReview { .. } => count += 1,
                Record::Review { .. } => return Err("complete review leaked".into()),
                _ => {}
            }
        }
        assert!(count > 0);
        current = path;
    }
    if std::env::var_os("PLASM_ARCHIVE_GRAPH_TEST_OUTPUT").is_none() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}
