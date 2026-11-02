use super::*;
use crate::workflow_learning::test_identities as ids;
use crate::workflow_learning::{
    catalogue_guidance::{CatalogueAssociation, CatalogueRequirementEvidence},
    normalization::FamilyMatch,
    teaching::{self, CheckedTeachingExample, ProgramObservation},
};

async fn reviewed(repo: &WorkflowRepository, scope: &LibraryScope, logical: &str) -> ClaimedReview {
    let logical = ids::logical(logical);
    let (session, source, bundle) = teaching::tests::sensitive_fixture();
    let observation = ProgramObservation::from_execution(&session, &source, &bundle);
    assert!(CheckedTeachingExample::from_observation(&observation).is_err());
    let requirements = CatalogueRequirementEvidence::from_compiled(&session, &bundle);
    assert!(requirements.available().is_some());
    repo.bind_session(scope, logical).await.unwrap();
    repo.record_execution(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        run_id: ids::run(&format!("run-{logical}")),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("fixture-execute"),
        run_artifact: serde_json::json!({"fixture":"trusted completed operation"}),
        source,
        compiled_plan: serde_json::to_value(&bundle.artifact().comp).unwrap(),
        observation,
        catalog_fingerprints: requirements
            .available()
            .unwrap()
            .catalog_fingerprints()
            .clone(),
        catalogue_requirements: requirements,
        symbol_context: Default::default(),
    })
    .await
    .unwrap();
    repo.review(&ReviewRequest {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        intent: "Create an item".into(),
        subtasks: vec![AchievedSubtask {
            intent: "Create an item".into(),
            observed_outcome: "Operation completed".into(),
            run_ids: vec![ids::run(&format!("run-{logical}"))],
        }],
    })
    .await
    .unwrap();
    repo.claim_scoped(60, Some(scope)).await.unwrap().unwrap()
}
fn association(claim: &ClaimedReview, purpose: &str) -> CatalogueAssociation {
    CatalogueAssociation::from_review(
        &claim.seal,
        &claim
            .seal
            .executions
            .iter()
            .map(|r| r.run_id)
            .collect::<Vec<_>>(),
        index::NormalizedIntent {
            purpose: purpose.into(),
            facets: vec![],
            aliases: vec![],
        },
        FamilyMatch::New,
        vec![1.0, 0.0],
        "guidance-fixture".into(),
    )
    .unwrap()
}
async fn search(
    repo: &WorkflowRepository,
    scope: &LibraryScope,
    association: &CatalogueAssociation,
    allowed: &crate::discovery_store::DiscoveryAuthorization,
    pins: &BTreeMap<String, String>,
) -> Vec<CatalogueAssociation> {
    repo.search_guidance(
        scope,
        GuidanceSearch {
            intent: &association.intent,
            embedding: &association.embedding,
            profile: &association.embedding_profile,
            allowed,
            catalog_fingerprints: pins,
            limit: 3,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn postgres_guidance_survives_withheld_teaching_and_is_fenced_bounded_authorized() {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        eprintln!("requires fresh PostgreSQL fixture database");
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("guidance_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let delimiter = if url.contains('?') { '&' } else { '?' };
    let mut repo =
        WorkflowRepository::connect(&format!("{url}{delimiter}options=-csearch_path%3D{schema}"))
            .await
            .unwrap();
    repo.migrate().await.unwrap();
    repo.tenant_cap = 1;
    let scope = LibraryScope {
        tenant: "guidance-tenant".into(),
        project: "first".into(),
    };
    let claim = reviewed(&repo, &scope, "first").await;
    let association = association(&claim, "create item");
    let pins = association.requirements.catalog_fingerprints().clone();
    let allowed =
        crate::discovery_store::DiscoveryAuthorization::catalogs(pins.keys().cloned().collect());
    let mut stale = claim.clone();
    stale.fence += 1;
    assert!(matches!(
        repo.finish_with_guidance(
            &stale,
            &[],
            std::slice::from_ref(&association),
            PublicationPolicy::Automatic
        )
        .await,
        Err(RepositoryError::LeaseConflict)
    ));
    let mut forged = association.clone();
    forged
        .evidence
        .insert(ids::run("invented-run"), "digest".into());
    assert!(repo
        .finish_with_guidance(&claim, &[], &[forged], PublicationPolicy::Automatic)
        .await
        .is_err());
    repo.finish_with_guidance(
        &claim,
        &[],
        std::slice::from_ref(&association),
        PublicationPolicy::Reviewed,
    )
    .await
    .unwrap();
    assert!(search(&repo, &scope, &association, &allowed, &pins)
        .await
        .is_empty());
    assert_eq!(
        repo.frozen_guidance(&scope, claim.id)
            .await
            .unwrap()
            .unwrap(),
        vec![association.clone()]
    );
    let inspection = repo.export_review_artifacts(&scope).await.unwrap();
    assert_eq!(inspection.len(), 1);
    let FrozenReviewArtifacts::Compatible {
        examples,
        catalogue_guidance,
        catalogue_guidance_digest,
        ..
    } = &inspection[0]
    else {
        panic!("current review is compatible");
    };
    assert!(examples.is_empty());
    assert_eq!(*catalogue_guidance, vec![association.clone()]);
    assert_eq!(catalogue_guidance_digest.len(), 64);
    let inspection_again = repo.export_review_artifacts(&scope).await.unwrap();
    assert_eq!(
        serde_json::to_value(&inspection).unwrap(),
        serde_json::to_value(&inspection_again).unwrap()
    );
    assert!(repo
        .export_review_artifacts(&LibraryScope {
            project: "unauthorized-project".into(),
            ..scope.clone()
        })
        .await
        .unwrap()
        .is_empty());
    repo.approve_publication(&scope, claim.id, "fixture-operator")
        .await
        .unwrap();
    let replay = repo
        .claim_recorded_scoped(60, Some(&scope))
        .await
        .unwrap()
        .unwrap();
    repo.finish(&replay, &[], PublicationPolicy::Reviewed)
        .await
        .unwrap();
    assert_eq!(
        search(&repo, &scope, &association, &allowed, &pins)
            .await
            .len(),
        1
    );
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    let duplicate = reviewed(&repo, &scope, "duplicate").await;
    let duplicate_association = self::association(&duplicate, "create item");
    repo.finish_with_guidance(
        &duplicate,
        &[],
        &[duplicate_association],
        PublicationPolicy::Automatic,
    )
    .await
    .unwrap();
    let mut connection = repo.pool.acquire().await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_learning_catalogue_guidance")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let support: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_learning_guidance_support")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(support, 2);
    drop(connection);
    let second_scope = LibraryScope {
        project: "second".into(),
        ..scope.clone()
    };
    let capped = reviewed(&repo, &second_scope, "capped").await;
    let candidate = self::association(&capped, "another intent");
    repo.finish_with_guidance(&capped, &[], &[candidate], PublicationPolicy::Automatic)
        .await
        .unwrap();
    assert!(search(&repo, &second_scope, &association, &allowed, &pins)
        .await
        .is_empty());
    let mut denied = allowed.clone();
    for catalog in &allowed.catalogs {
        denied.capabilities.insert(catalog.clone(), BTreeSet::new());
    }
    assert!(search(&repo, &scope, &association, &denied, &pins)
        .await
        .is_empty());
    assert!(
        search(&repo, &scope, &association, &allowed, &BTreeMap::new())
            .await
            .is_empty()
    );
    let left_scope = LibraryScope {
        tenant: "concurrent-tenant".into(),
        project: "left".into(),
    };
    let right_scope = LibraryScope {
        project: "right".into(),
        ..left_scope.clone()
    };
    let left = reviewed(&repo, &left_scope, "left").await;
    let right = reviewed(&repo, &right_scope, "right").await;
    let left_assoc = [self::association(&left, "left")];
    let right_assoc = [self::association(&right, "right")];
    let (left_result, right_result) = tokio::join!(
        repo.finish_with_guidance(&left, &[], &left_assoc, PublicationPolicy::Automatic),
        repo.finish_with_guidance(&right, &[], &right_assoc, PublicationPolicy::Automatic)
    );
    left_result.unwrap();
    right_result.unwrap();
    let mut connection = repo.pool.acquire().await.unwrap();
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM workflow_learning_catalogue_guidance WHERE tenant='concurrent-tenant'").fetch_one(&mut connection).await.unwrap();
    assert_eq!(count, 1);
    drop(connection);
    // Profiles form separate variants: old profile rows remain bounded evidence,
    // while a fresh reviewed observation can supply the current embedding space.
    repo.tenant_cap = 2;
    let profile_scope = LibraryScope {
        tenant: "profile-tenant".into(),
        project: "profiles".into(),
    };
    let first_profile = reviewed(&repo, &profile_scope, "profile-first").await;
    let first_association = self::association(&first_profile, "create item");
    repo.finish_with_guidance(
        &first_profile,
        &[],
        std::slice::from_ref(&first_association),
        PublicationPolicy::Automatic,
    )
    .await
    .unwrap();
    let second_profile = reviewed(&repo, &profile_scope, "profile-second").await;
    let second_association = CatalogueAssociation::from_review(
        &second_profile.seal,
        &second_profile
            .seal
            .executions
            .iter()
            .map(|r| r.run_id)
            .collect::<Vec<_>>(),
        first_association.intent.clone(),
        FamilyMatch::New,
        vec![1.0, 0.0],
        "guidance-profile-v2".into(),
    )
    .unwrap();
    assert_ne!(first_association.variant, second_association.variant);
    assert!(
        search(&repo, &profile_scope, &second_association, &allowed, &pins)
            .await
            .is_empty()
    );
    repo.finish_with_guidance(
        &second_profile,
        &[],
        std::slice::from_ref(&second_association),
        PublicationPolicy::Automatic,
    )
    .await
    .unwrap();
    assert_eq!(
        search(&repo, &profile_scope, &second_association, &allowed, &pins)
            .await
            .len(),
        1
    );

    // Migration retains incompatible source bytes and fences old jobs instead
    // of trying to deserialize a parameterized template as an authored example.
    let old_example = Uuid::new_v4();
    let queued_review = Uuid::new_v4();
    let completed_review = Uuid::new_v4();
    let old_payload = serde_json::json!({"state":{"state":"verified","evidence":{"program":{"program":{"source":"old template"}}}}});
    let old_seal = serde_json::json!({"executions":[{"portable_program":{"state":"checked"}}]});
    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state) VALUES($1,$2,$3,'old','old','old',$4,'active')")
        .bind(old_example).bind(&scope.tenant).bind(&scope.project).bind(&old_payload).execute(&mut connection).await.unwrap();
    for (id, state) in [(queued_review, "queued"), (completed_review, "completed")] {
        sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal,state) VALUES($1,$2,$3,$4,1,'old',$5,$6)")
            .bind(id).bind(&scope.tenant).bind(&scope.project).bind(id.to_string()).bind(&old_seal).bind(state).execute(&mut connection).await.unwrap();
    }
    sqlx::query("INSERT INTO workflow_learning_proposal_batches(review_id,artifact_hashes) VALUES($1,'[]'::jsonb)")
        .bind(completed_review).execute(&mut connection).await.unwrap();
    sqlx::query("INSERT INTO workflow_learning_publication_approvals(review_id,tenant,project,actor) VALUES($1,$2,$3,'historical-operator')")
        .bind(queued_review).bind(&scope.tenant).bind(&scope.project).execute(&mut connection).await.unwrap();
    drop(connection);
    repo.migrate().await.unwrap();
    assert!(matches!(
        repo.approve_publication(&scope, queued_review, "historical-operator")
            .await,
        Err(RepositoryError::IncompatibleTeachingFormat)
    ));
    assert!(matches!(
        repo.approve_publication(&scope, completed_review, "new-operator")
            .await,
        Err(RepositoryError::IncompatibleTeachingFormat)
    ));
    let mixed_inspection = repo.export_review_artifacts(&scope).await.unwrap();
    assert!(mixed_inspection.iter().any(|entry| matches!(entry,FrozenReviewArtifacts::Compatible{review_ref,..} if *review_ref==claim.id)));
    assert!(mixed_inspection.iter().any(|entry| matches!(entry,FrozenReviewArtifacts::Incompatible{review_ref,reason:ReviewInspectionIncompatibility::IncompatibleTeachingFormat,..} if *review_ref==completed_review)));
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    assert_eq!(
        search(&repo, &scope, &association, &allowed, &pins)
            .await
            .len(),
        1
    );
    assert!(matches!(
        repo.publish_frozen(&scope, completed_review, &[], "operator")
            .await,
        Err(RepositoryError::IncompatibleTeachingFormat)
    ));
    let mut connection = repo.pool.acquire().await.unwrap();
    let (state, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT state,candidate FROM workflow_learning_examples WHERE id=$1")
            .bind(old_example)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(state, "bad");
    assert_eq!(payload, old_payload);
    let (state, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT state,seal FROM workflow_learning_reviews WHERE id=$1")
            .bind(queued_review)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(payload, old_seal);
    // Valid authored format alone is insufficient: version one did not carry
    // nominal-symbol authority and must never be rebound as a current example.
    let version_one_payload = serde_json::json!({"state":{"state":"verified","evidence":{"program":{"program":{"format":"whole_program_example","schema_version":1,"source":"retained authored source"}}}}});
    let mut version_one_seal = serde_json::to_value(&claim.seal).unwrap();
    *version_one_seal
        .pointer_mut("/executions/0/observation/value/program/schema_version")
        .expect("actual observed teaching program") = serde_json::json!(1);
    let mut old_versions = Vec::new();
    for state in ["active", "candidate", "superseded"] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state) VALUES($1,$2,$3,'version-one','version-one',$4,$5,$6)")
            .bind(id).bind(&scope.tenant).bind(&scope.project).bind(id.to_string()).bind(&version_one_payload).bind(state).execute(&mut connection).await.unwrap();
        old_versions.push(id);
    }
    let mut old_reviews = Vec::new();
    for state in ["queued", "leased", "awaiting_approval", "completed"] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO workflow_learning_reviews(id,tenant,project,logical_session,session_revision,digest,seal,state) VALUES($1,$2,$3,$4,1,'version-one',$5,$6)")
            .bind(id).bind(&scope.tenant).bind(&scope.project).bind(id.to_string()).bind(&version_one_seal).bind(state).execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO workflow_learning_proposal_batches(review_id,artifact_hashes) VALUES($1,'[]'::jsonb)")
            .bind(id).execute(&mut connection).await.unwrap();
        old_reviews.push((id, state));
    }
    drop(connection);
    repo.migrate().await.unwrap();
    let inspection = repo.export_review_artifacts(&scope).await.unwrap();
    for (id, previous_state) in old_reviews {
        assert!(matches!(
            repo.approve_publication(&scope, id, "operator").await,
            Err(RepositoryError::IncompatibleTeachingFormat)
        ));
        assert!(matches!(
            repo.publish_frozen(&scope, id, &[], "operator").await,
            Err(RepositoryError::IncompatibleTeachingFormat)
        ));
        assert!(inspection.iter().any(|entry| matches!(entry, FrozenReviewArtifacts::Incompatible { review_ref, .. } if *review_ref == id)));
        let mut connection = repo.pool.acquire().await.unwrap();
        let (state, payload): (String, serde_json::Value) =
            sqlx::query_as("SELECT state,seal FROM workflow_learning_reviews WHERE id=$1")
                .bind(id)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(
            state,
            if previous_state == "completed" {
                "completed"
            } else {
                "failed"
            }
        );
        assert_eq!(payload, version_one_seal);
    }
    for id in old_versions {
        let mut connection = repo.pool.acquire().await.unwrap();
        let (state, reason, payload): (String, String, serde_json::Value) = sqlx::query_as(
            "SELECT state,reason,candidate FROM workflow_learning_examples WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(state, "bad");
        assert_eq!(reason, "incompatible teaching format; retained for repair");
        assert_eq!(payload, version_one_payload);
    }
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    assert_eq!(
        search(&repo, &scope, &association, &allowed, &pins)
            .await
            .len(),
        1
    );
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
