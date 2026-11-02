use super::tests::{tiny_engine, write_native_workflow_fixture, MockProductListTransport};
use super::*;
use plasm_agent_core::workflow_learning::{
    archive::{
        ArchiveDigest, ArchiveExportRequest, ArchiveKind, ArchiveRecord, AssessmentUnavailable,
        CollectionIdentity, TaskAssessment, TaskMembership, TaskOutcome,
    },
    index::NormalizedIntent,
    model::{Candidate, CandidateState, PublicationPolicy, Quality, VerifiedProgram},
    repository::WorkflowRepository,
    teaching::CheckedTeachingExample,
};

async fn evidence_counts(
    repository: &WorkflowRepository,
    scope: &plasm_agent_core::workflow_learning::model::LibraryScope,
) -> (usize, usize) {
    let archive = repository
        .export_archive(
            scope,
            &ArchiveExportRequest {
                kind: ArchiveKind::PrivateCollection,
                collection: CollectionIdentity {
                    protocol_digest: ArchiveDigest::try_from("a".repeat(64)).unwrap(),
                    tasks: vec![TaskMembership {
                        task_id: "fixture".into(),
                        group_id: "fixture".into(),
                    }],
                },
            },
        )
        .await
        .unwrap();
    let items = &archive.document().items;
    (
        items
            .iter()
            .filter(|item| {
                matches!(
                    item.record,
                    ArchiveRecord::Observation { .. } | ArchiveRecord::HistoricalObservation { .. }
                )
            })
            .count(),
        items
            .iter()
            .filter(|item| {
                matches!(
                    item.record,
                    ArchiveRecord::Review { .. } | ArchiveRecord::HistoricalReview { .. }
                )
            })
            .count(),
    )
}

#[tokio::test]
async fn postgres_library_probe_uses_serving_without_executions_and_rejects_contract_drift() {
    let (Ok(url), Ok(_)) = (
        std::env::var("PLASM_TEST_PROBE_POSTGRES_URL"),
        std::env::var("PLASM_DISCOVERY_DATABASE_URL"),
    ) else {
        eprintln!(
            "library probe integration requires isolated workflow and pgvector discovery databases"
        );
        return;
    };
    let (mut producer, info) = tiny_engine();
    producer.session_id =
        plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string();
    let config = NativeLearningConfig::Collect(
        plasm_agent_core::workflow_learning::native::NativeLibraryConfig {
            scope: plasm_agent_core::workflow_learning::model::LibraryScope {
                tenant: format!("probe-{}", producer.session_id),
                project: "fixture".into(),
            },
            database_url: url,
        },
    );
    producer
        .configure_workflow_learning(config.clone())
        .unwrap();
    producer
        .expose_seeds(
            "Inspect products",
            &[CapabilitySeed {
                entry_id: info.entry_id,
                entity: "Product".into(),
            }],
        )
        .await
        .unwrap();
    let source = "class Inspect(Program):\n    def build(self):\n        return e1.query()\n";
    let plan = producer.dry_run(source).await.unwrap();
    assert!(
        producer
            .run_plan_live(&plan.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap()
            .ok
    );
    let index: serde_json::Value =
        serde_json::from_str(&producer.completed_workflow_runs().await.unwrap()).unwrap();
    let run = index["runs"][0]["run_id"].as_str().unwrap();
    producer.review_workflow(&serde_json::json!({"subtasks":[{"intent":"Inspect products","observed_outcome":"Observed fixture product","run_ids":[run]}]}).to_string()).await.unwrap();
    let repository = config.learning().repository().await.unwrap();
    let scope = config.scope().unwrap();
    let claim = repository.claim(120).await.unwrap().unwrap();
    let receipt = &claim.seal.executions[0];
    let checked = CheckedTeachingExample::from_observation(&receipt.observation).unwrap();
    let candidate = Candidate {
        intent: NormalizedIntent {
            purpose: "Inspect products".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "fixture".into(),
        family: "inspect".into(),
        variant: "matrix".into(),
        identity: checked.program().structural_fingerprint.clone(),
        coverage: vec!["products".into()],
        quality: Quality::observed(&claim.seal, &[receipt.run_id], 1).unwrap(),
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(checked, claim.seal.assurance),
        },
        catalog_fingerprints: receipt.catalog_fingerprints.clone(),
        expected_family_version: 0,
    };
    repository
        .finish(&claim, &[candidate], PublicationPolicy::Automatic)
        .await
        .unwrap();
    repository
        .record_archive_assessment(
            &scope,
            &TaskAssessment {
                attempt_id: "fixture".into(),
                task_id: "fixture".into(),
                group_id: "fixture".into(),
                logical_sessions: vec![
                    plasm_agent_core::session_identity::LogicalSessionId::parse(
                        &producer.session_id,
                    )
                    .unwrap(),
                ],
                protocol_digest: ArchiveDigest::try_from("a".repeat(64)).unwrap(),
                assessor: "native-offline-fixture".into(),
                outcome: TaskOutcome::Unavailable {
                    reason: AssessmentUnavailable::NotAttempted,
                },
            },
        )
        .await
        .unwrap();
    if let Ok(output) = std::env::var("PLASM_SERVING_PROBE_FIXTURE_OUT") {
        let output = std::path::Path::new(&output);
        assert!(!output.exists(), "fixture output must be new");
        write_native_workflow_fixture(output, &producer);
        let archive = repository
            .export_archive(
                &scope,
                &ArchiveExportRequest {
                    kind: ArchiveKind::PrivateCollection,
                    collection: CollectionIdentity {
                        protocol_digest: ArchiveDigest::try_from("a".repeat(64)).unwrap(),
                        tasks: vec![TaskMembership {
                            task_id: "fixture".into(),
                            group_id: "fixture".into(),
                        }],
                    },
                },
            )
            .await
            .unwrap();
        std::fs::write(output.join("archive.json"), archive.encode().unwrap()).unwrap();
    }
    let before = evidence_counts(&repository, &scope).await;
    assert_eq!(before, (1, 1));
    let directory = std::env::temp_dir().join(format!(
        "plasm-serving-proof-{}",
        plasm_agent_core::session_identity::LogicalSessionId::new_v4()
    ));
    for changed in [false, true] {
        if changed {
            let cgs = Arc::make_mut(producer.catalogs.get_mut("matrix").unwrap());
            cgs.capabilities
                .get_mut("product_list")
                .unwrap()
                .description = "Changed semantic catalogue contract".into();
            producer.compiled_catalogs.insert(
                "matrix".into(),
                Arc::new(plasm_compile::compile_cgs_capability_templates(cgs).unwrap()),
            );
        }
        let path = directory.join(if changed { "changed" } else { "original" });
        write_native_workflow_fixture(&path, &producer);
        let owner = crate::PlasmEngine::new();
        owner
            .configure_workflow_learning(serde_json::to_string(&config).unwrap())
            .await
            .unwrap();
        owner
            .load_catalog(
                path.join("matrix.manifest.json")
                    .to_string_lossy()
                    .into_owned(),
            )
            .await
            .unwrap();
        owner
            .activate_discovery(
                format!("proof-{}-{changed}", producer.session_id),
                serde_json::to_string(&plasm_core::prerequisites::DeploymentBindings::default())
                    .unwrap(),
            )
            .await
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_str(&owner.verify_workflow_library(1).await.unwrap()).unwrap();
        assert_eq!(report["examples"].as_array().unwrap().len(), 1);
        assert_eq!(
            report["examples"][0]["outcome"]["status"],
            if changed { "incompatible" } else { "checked" },
            "{report}"
        );
        assert!(report["examples"][0]["outcome"].get("source").is_none());
        assert_eq!(evidence_counts(&repository, &scope).await, before);
        assert!(owner.inner.lock().await.execute_session.is_none());
        assert!(crate::lock(&owner.sessions).is_empty());
        owner.close().await;
    }
    producer.python_pool.close().await;
    std::fs::remove_dir_all(directory).unwrap();
}
