use super::*;
use crate::workflow_learning::test_identities as ids;

async fn review(
    repo: &WorkflowRepository,
    scope: &LibraryScope,
    logical: &str,
) -> Result<ReviewHandle> {
    let logical = ids::logical(logical);
    let checked = crate::workflow_learning::teaching::tests::checked_fixture();
    repo.bind_session(scope, logical).await?;
    repo.begin_execution(scope, logical, "attempt").await?;
    repo.record_execution(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        run_id: ids::run("run"),
        plan_id: ids::plan("plan"),
        prompt_hash: ids::prompt("prompt"),
        execute_session: ids::execute("execute"),
        run_artifact: serde_json::json!({"complete":true}),
        source: checked.program().source.clone(),
        symbol_context: Default::default(),
        catalogue_requirements: Default::default(),
        compiled_plan: serde_json::json!({}),
        observation: checked.observation(),
        catalog_fingerprints: checked.program().catalog_fingerprints.clone(),
    })
    .await?;
    repo.finish_execution(scope, logical, "attempt", true)
        .await?;
    repo.review(&ReviewRequest {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        intent: "inspect".into(),
        subtasks: vec![AchievedSubtask {
            intent: "inspect".into(),
            observed_outcome: "item observed".into(),
            run_ids: vec![ids::run("run")],
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
    let other = LibraryScope {
        tenant: "other-tenant".into(),
        project: scope.project.clone(),
    };
    assert!(repo.export_proposals(&other).await?.is_empty());
    assert!(matches!(
        repo.publish_frozen(&other, handle.id, &hashes, "operator")
            .await,
        Err(RepositoryError::ApprovalUnavailable)
    ));
    assert!(matches!(
        repo.publish_frozen(&scope, handle.id, &hashes[..1], "operator")
            .await,
        Err(RepositoryError::ImmutableConflict)
    ));
    let receipt = repo
        .publish_frozen(&scope, handle.id, &hashes, "operator")
        .await?;
    assert_eq!(receipt.artifact_hashes, hashes);
    let repeated = repo
        .publish_frozen(&scope, handle.id, &hashes, "operator")
        .await?;
    assert_eq!(receipt.publication_order, repeated.publication_order);
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
        repo.publish_frozen(&scope, handle.id, &hashes, "operator")
            .await,
        Err(RepositoryError::ApprovalUnavailable)
    ));
    sqlx::query("UPDATE workflow_learning_examples SET state='superseded' WHERE id=$1")
        .bind(active[0].id)
        .execute(&mut repo.pool.acquire().await?)
        .await?;
    assert!(matches!(
        repo.publish_frozen(&scope, handle.id, &hashes, "operator")
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
