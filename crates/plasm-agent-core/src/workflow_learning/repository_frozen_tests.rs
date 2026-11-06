use super::*;
use crate::workflow_learning::test_identities as ids;

#[tokio::test]
async fn grant_expiry_while_publication_waits_rolls_back_decision(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return Ok(());
    };
    let repo = WorkflowRepository::connect(&url).await?;
    repo.migrate().await?;
    let scope = LibraryScope {
        tenant: Uuid::new_v4().to_string(),
        project: "expiry-during-publication".into(),
    };
    let handle = review(&repo, &scope, &Uuid::new_v4().to_string()).await?;
    let claim = repo.claim_scoped(60, Some(&scope)).await?.unwrap();
    sqlx::query("INSERT INTO workflow_learning_tenants(tenant) VALUES($1)")
        .bind(&scope.tenant)
        .execute(&mut repo.pool.acquire().await?)
        .await?;
    sqlx::query("UPDATE workflow_training_grants SET expires_at=clock_timestamp()+interval '2 seconds' WHERE tenant=$1").bind(&scope.tenant).execute(&mut repo.pool.acquire().await?).await?;
    let mut blocker = repo.pool.begin().await?;
    sqlx::query("SELECT id FROM workflow_learning_reviews WHERE id=$1 FOR UPDATE")
        .bind(handle.id)
        .fetch_one(&mut blocker.executor())
        .await?;
    let release = async move {
        tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
        blocker.commit().await
    };
    let (finished, released) = tokio::join!(
        repo.finish(&claim, &[], PublicationPolicy::Automatic),
        release
    );
    released?;
    assert!(matches!(
        finished,
        Err(RepositoryError::Training(
            training::TrainingError::GrantUnavailable | training::TrainingError::Held
        ))
    ));
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workflow_training_automatic_decisions WHERE review_id=$1)",
    )
    .bind(handle.id)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert!(!exists);
    Ok(())
}

async fn review(
    repo: &WorkflowRepository,
    scope: &LibraryScope,
    logical: &str,
) -> Result<ReviewHandle> {
    let logical = ids::logical(logical);
    let checked = crate::workflow_learning::teaching::tests::checked_fixture();
    repo.bind_training_fixture(scope, logical).await?;
    repo.begin_execution(scope, logical, "attempt").await?;
    repo.record_training_fixture(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        execution_id: ids::run("run"),
        plan_id: ids::plan("plan"),
        prompt_hash: ids::prompt("prompt"),
        execute_session: ids::execute("execute"),
        source: checked.program().source.clone(),
        teaching_plan: crate::workflow_learning::review_packet::TeachingPlan::Unavailable {
            reason: crate::workflow_learning::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },

        catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
        compiled_plan: serde_json::json!({}),
        observation: checked.observation(),
        catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
    })
    .await?;
    repo.finish_execution(scope, logical, "attempt", true)
        .await?;
    repo.review_training_fixture(&ReviewRequest {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        intent: "inspect".into(),
        subtasks: vec![AchievedSubtask {
            intent: "inspect".into(),
            observed_outcome: "item observed".into(),
            execution_ids: vec![ids::run("run")],
        }],
    })
    .await
}

#[tokio::test]
async fn postgres_frozen_proposals_preserve_uncertainty_and_publish_without_model_calls(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        eprintln!("PLASM_TEST_POSTGRES_URL absent: frozen publication integration not exercised");
        return Ok(());
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let schema = format!("frozen_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await?;
    let separator = if url.contains('?') { '&' } else { '?' };
    let repo =
        WorkflowRepository::connect(&format!("{url}{separator}options=-csearch_path%3D{schema}"))
            .await?;
    repo.migrate().await?;
    let scope = LibraryScope {
        tenant: "tenant".into(),
        project: "collection".into(),
    };
    let handle = review(&repo, &scope, "logical").await?;
    let claim = repo
        .claim_scoped(60, Some(&scope))
        .await?
        .expect("own review");
    let checked = crate::workflow_learning::teaching::CheckedTeachingExample::from_observation(
        &claim.seal.executions[0].observation,
    )
    .unwrap();
    let candidate = Candidate {
        intent: crate::workflow_learning::index::NormalizedIntent {
            purpose: "inspect".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "fixture".into(),
        family: "inspect".into(),
        variant: "matrix".into(),
        identity: checked.program().structural_fingerprint.clone(),
        coverage: vec!["read".into()],
        quality: Quality::observed(&claim.seal, &[ids::run("run")], 1)?,
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(checked.clone(), claim.seal.assurance),
        },
        catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
        expected_family_version: 0,
    };
    let retained_id = Uuid::new_v4();
    let mut sensitive = candidate.clone();
    let credential = "credential-echo-must-not-display";
    sensitive.intent.purpose = credential.into();
    sensitive.intent.aliases = vec![credential.into()];
    sensitive.intent.facets = vec![credential.into()];
    sensitive.family = credential.into();
    sensitive.variant = credential.into();
    sensitive.identity = credential.into();
    sensitive.coverage = vec![credential.into()];
    sensitive.embedding_profile = credential.into();
    sensitive.state = CandidateState::Proposed {
        diagnostic: ProposalDiagnostic {
            kind: DiagnosticKind::SensitiveOperand,
            message: credential.into(),
        },
    };
    let preserved = serde_json::to_value(&sensitive)?;
    redact_candidate_presentation(&mut sensitive);
    assert!(!serde_json::to_string(&sensitive)?.contains(credential));
    assert!(preserved.to_string().contains(credential));
    let mut retained = serde_json::to_value(&candidate)?;
    *retained
        .pointer_mut("/state/evidence/program/program/schema_version")
        .expect("verified program version") = serde_json::json!(2);
    sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state,reason) VALUES($1,$2,$3,$4,$5,$6,$7,'bad','incompatible teaching format; retained for repair')")
        .bind(retained_id).bind(&scope.tenant).bind(&scope.project).bind(&candidate.family)
        .bind(&candidate.variant).bind(&candidate.identity).bind(&retained)
        .execute(&mut repo.pool.acquire().await?).await?;
    let mut uncertain = candidate.clone();
    uncertain.family = "uncertain-purpose".into();
    uncertain.state = CandidateState::Uncertain {
        program: checked,
        diagnostic: ProposalDiagnostic {
            kind: DiagnosticKind::SemanticUncertainty,
            message: "meaning unresolved".into(),
        },
    };
    repo.finish(&claim, &[candidate, uncertain], PublicationPolicy::Shadow)
        .await?;
    assert!(repo.list_eligible(&scope, 10).await?.is_empty());
    let artifacts = repo.export_proposals(&scope).await?;
    assert_eq!(artifacts.len(), 2);
    assert!(artifacts
        .iter()
        .any(|artifact| matches!(artifact.candidate.state, CandidateState::Uncertain { .. })));
    let hashes = artifacts
        .iter()
        .map(|artifact| artifact.candidate_hash.clone())
        .collect::<Vec<_>>();
    let uncertain_ref = artifacts
        .iter()
        .find(|artifact| matches!(artifact.candidate.state, CandidateState::Uncertain { .. }))
        .unwrap();
    let host = training::TrustedTrainingActor::trusted_host(
        scope.clone(),
        "fixture-host".into(),
        "fixture-host-key".into(),
    )?;
    let owner = training::TrustedTrainingActor::human_owner(
        scope.clone(),
        "owner".into(),
        "owner-key".into(),
    )?;
    let logical = claim.seal.request.logical_session;
    let status = repo.training_status(&scope, logical).await?;
    let mut selection = training::TrainingCandidateSelection {
        review_ref: handle.id,
        candidate_hash: uncertain_ref.candidate_hash.clone(),
        guidance_digest: plasm_core::catalog_discovery::content_hash(b"[]"),
        action: training::CandidateDecisionAction::Approve,
        outcome_confirmation: training::HumanOutcomeConfirmation::NotProvided,
    };
    let pending = repo
        .request_candidate_decision(
            &host,
            logical,
            status.revision,
            Uuid::new_v4(),
            vec![selection.clone()],
        )
        .await?;
    selection.action = training::CandidateDecisionAction::Reject {
        reason: "Purpose is unresolved".into(),
    };
    let key = Uuid::new_v4();
    let resolve = || {
        repo.resolve_training_candidate_request(
            &owner,
            pending.request_ref,
            pending.revision,
            key,
            std::slice::from_ref(&selection),
            true,
        )
    };
    let (first, concurrent) = tokio::join!(resolve(), resolve());
    let decision = first?;
    assert_eq!(
        serde_json::to_value(&decision)?,
        serde_json::to_value(concurrent?)?
    );
    assert!(matches!(
        decision.decisions[0].outcome,
        CandidateDecisionOutcome::Rejected
    ));
    let repeated = repo
        .resolve_training_candidate_request(
            &owner,
            pending.request_ref,
            pending.revision,
            key,
            std::slice::from_ref(&selection),
            true,
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&decision)?,
        serde_json::to_value(&repeated)?
    );
    let summaries = repo
        .list_training_candidates(&scope, logical, None, 32)
        .await?;
    let status = repo.training_status(&scope, logical).await?;
    let pending_decline = repo
        .request_candidate_decision(
            &host,
            logical,
            status.revision,
            Uuid::new_v4(),
            vec![selection.clone()],
        )
        .await?;
    let decline_key = Uuid::new_v4();
    let declined = repo
        .resolve_training_candidate_request(
            &owner,
            pending_decline.request_ref,
            pending_decline.revision,
            decline_key,
            &[],
            false,
        )
        .await?;
    assert!(declined.decisions.is_empty() && declined.publications.is_empty());
    let repeated = repo
        .resolve_training_candidate_request(
            &owner,
            pending_decline.request_ref,
            pending_decline.revision,
            decline_key,
            &[],
            false,
        )
        .await?;
    assert_eq!(
        serde_json::to_value(declined)?,
        serde_json::to_value(repeated)?
    );
    assert!(summaries
        .candidates
        .iter()
        .any(
            |entry| entry.reference.candidate_hash == selection.candidate_hash
                && matches!(entry.disposition, TrainingCandidateDisposition::Rejected)
        ));
    assert!(summaries
        .candidates
        .iter()
        .any(
            |entry| entry.reference.candidate_hash != selection.candidate_hash
                && matches!(
                    entry.disposition,
                    TrainingCandidateDisposition::AwaitingDecision
                )
        ));
    let other = LibraryScope {
        tenant: "other-tenant".into(),
        project: scope.project.clone(),
    };
    assert!(repo.export_proposals(&other).await?.is_empty());
    assert!(matches!(
        repo.publish_training_fixture(&other, handle.id, &hashes, "operator")
            .await,
        Err(RepositoryError::Unavailable)
    ));
    repo.publish_training_fixture(&scope, handle.id, &hashes[..1], "operator")
        .await?;
    let receipt = repo
        .publish_training_fixture(&scope, handle.id, &hashes, "operator")
        .await?;
    assert_eq!(receipt.artifact_hashes, hashes);
    let repeated = repo
        .publish_training_fixture(&scope, handle.id, &hashes, "operator")
        .await?;
    assert_eq!(receipt.publication_order, repeated.publication_order);
    let delegated = repo
        .publish_frozen(
            &scope,
            handle.id,
            &hashes[1..],
            &plasm_core::catalog_discovery::content_hash(b"[]"),
            &host,
        )
        .await?;
    let authority: serde_json::Value = sqlx::query_scalar(
        "SELECT authority FROM workflow_training_publications WHERE publication_order=$1",
    )
    .bind(delegated.publication_order)
    .fetch_one(&mut repo.pool.acquire().await?)
    .await?;
    assert!(matches!(
        serde_json::from_value::<PublicationAuthority>(authority)?,
        PublicationAuthority::Delegated { .. }
    ));
    let after = repo.export_proposals(&scope).await?;
    assert_eq!(
        serde_json::to_value(&artifacts)?,
        serde_json::to_value(&after)?
    );
    let active = repo.list_eligible(&scope, 10).await?;
    assert_eq!(active.len(), 1);
    assert_ne!(active[0].id, retained_id);
    let (state, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT state,candidate FROM workflow_learning_examples WHERE id=$1")
            .bind(retained_id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!(state, "bad");
    assert_eq!(
        payload, retained,
        "new checked authority never rewrites retained evidence"
    );

    let supports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_learning_support WHERE review_id=$1")
            .bind(handle.id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!(
        supports, 1,
        "uncertain candidates must be auditable without inventing support"
    );
    repo.quarantine(&scope, active[0].id, "fixture drift")
        .await?;
    assert!(matches!(
        repo.publish_training_fixture(&scope, handle.id, &hashes, "operator")
            .await,
        Err(RepositoryError::ApprovalUnavailable)
    ));
    sqlx::query("UPDATE workflow_learning_examples SET state='superseded' WHERE id=$1")
        .bind(active[0].id)
        .execute(&mut repo.pool.acquire().await?)
        .await?;
    assert!(matches!(
        repo.publish_training_fixture(&scope, handle.id, &hashes, "operator")
            .await,
        Err(RepositoryError::ApprovalUnavailable)
    ));

    let other_review = review(&repo, &other, "other-logical").await?;
    let other_claim = repo.claim_scoped(60, Some(&other)).await?.unwrap();
    assert_eq!(other_claim.id, other_review.id);
    sqlx::query("UPDATE workflow_learning_reviews SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(other_review.id).execute(&mut repo.pool.acquire().await?).await?;
    assert!(repo.claim_scoped(60, Some(&scope)).await?.is_none());
    let state: String =
        sqlx::query_scalar("SELECT state FROM workflow_learning_reviews WHERE id=$1")
            .bind(other_review.id)
            .fetch_one(&mut repo.pool.acquire().await?)
            .await?;
    assert_eq!(
        state, "leased",
        "scoped claim must not recover another tenant's expired job"
    );
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await?;
    Ok(())
}
