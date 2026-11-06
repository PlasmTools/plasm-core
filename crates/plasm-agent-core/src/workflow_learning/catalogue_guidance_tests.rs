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
    repo.bind_training_fixture(scope, logical).await.unwrap();
    repo.record_training_fixture(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        execution_id: ids::run(&format!("run-{logical}")),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("fixture-execute"),
        source,
        compiled_plan: serde_json::to_value(&bundle.artifact().comp).unwrap(),
        observation,
        catalog_fingerprints: requirements
            .available()
            .unwrap()
            .catalog_fingerprints()
            .clone(),
        catalogue_requirements: requirements,
        teaching_plan: crate::workflow_learning::review_packet::TeachingPlan::Unavailable {
            reason:
                crate::workflow_learning::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },
    })
    .await
    .unwrap();
    repo.review_training_fixture(&ReviewRequest {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        intent: "Create an item".into(),
        subtasks: vec![AchievedSubtask {
            intent: "Create an item".into(),
            observed_outcome: "Operation completed".into(),
            execution_ids: vec![ids::run(&format!("run-{logical}"))],
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
            .map(|r| r.execution_id)
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
    assert!(repo
        .approve_training_fixture(&scope, claim.id, "fixture-operator")
        .await
        .is_err());
    assert!(
        search(&repo, &scope, &association, &allowed, &pins)
            .await
            .is_empty(),
        "guidance cannot be published without selected whole-program evidence"
    );
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
