use super::tests::{
    opt_in_training_with_policy, review_request, tiny_engine, write_native_workflow_fixture,
    MockProductListTransport,
};
use super::*;
use plasm_agent_core::workflow_learning::{
    archive::ArchiveKind,
    filesystem_archive::{Reader, Record, RecordKind},
    index::NormalizedIntent,
    model::{Candidate, CandidateState, PublicationPolicy, Quality, VerifiedProgram},
    repository::WorkflowRepository,
    teaching::CheckedTeachingExample,
};

async fn evidence_counts(
    repository: &WorkflowRepository,
    scope: &plasm_agent_core::workflow_learning::model::LibraryScope,
) -> (usize, usize) {
    let path = std::env::temp_dir().join(format!(
        "plasm-evidence-counts-{}",
        plasm_agent_core::session_identity::LogicalSessionId::new_v4()
    ));
    let manifest = repository
        .export_filesystem_archive(scope, &path, ArchiveKind::PrivateCollection)
        .await
        .unwrap();
    let observations = manifest
        .records
        .values()
        .filter(|record| record.kind == RecordKind::Observation)
        .count();
    let reviews = manifest
        .records
        .values()
        .filter(|record| record.kind == RecordKind::Review)
        .count();
    std::fs::remove_dir_all(path).unwrap();
    (observations, reviews)
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
            serve_examples: false,
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
    let actor = opt_in_training_with_policy(&producer, true).await;
    let plan = producer.dry_run(source).await.unwrap();
    assert!(
        producer
            .run_plan_live(&plan.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap()
            .ok
    );
    let index: serde_json::Value =
        serde_json::from_str(&producer.completed_workflow_executions().await.unwrap()).unwrap();
    let run = index["executions"][0]["execution_id"].as_str().unwrap();
    producer.review_workflow(&review_request(serde_json::json!([{"intent":"Inspect products","observed_outcome":"Observed fixture product","execution_ids":[run]}])).to_string(),actor).await.unwrap();
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
        quality: Quality::observed(&claim.seal, &[receipt.execution_id], 1).unwrap(),
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
    if let Ok(output) = std::env::var("PLASM_SERVING_PROBE_FIXTURE_OUT") {
        let output = std::path::Path::new(&output);
        assert!(!output.exists(), "fixture output must be new");
        write_native_workflow_fixture(output, &producer);
        repository
            .export_filesystem_archive(
                &scope,
                &output.join("archive"),
                ArchiveKind::PrivateCollection,
            )
            .await
            .unwrap();
    }
    let before = evidence_counts(&repository, &scope).await;
    assert_eq!(before, (1, 1));
    let directory = std::env::temp_dir().join(format!(
        "plasm-serving-proof-{}",
        plasm_agent_core::session_identity::LogicalSessionId::new_v4()
    ));
    // A different target must explicitly attest the exact source review before
    // a compiler-checked example may be adopted. Reimport is idempotent.
    let fixture = directory.join("import-fixture");
    write_native_workflow_fixture(&fixture, &producer);
    let archive_path = directory.join("source-archive");
    let manifest = repository
        .export_filesystem_archive(&scope, &archive_path, ArchiveKind::PrivateCollection)
        .await
        .unwrap();
    let reader = Reader::open(&archive_path).unwrap();
    let evidence = manifest
        .records
        .keys()
        .find_map(|identity| match reader.read_record(identity).unwrap() {
            Record::ExampleSupport { evidence, .. } => Some(evidence),
            _ => None,
        })
        .expect("published example support");
    let mut target_config = config.clone();
    if let NativeLearningConfig::Collect(ref mut target) = target_config {
        target.scope.project = format!("import-{}", producer.session_id);
    } else {
        panic!("expected collecting target");
    }
    let target = crate::PlasmEngine::new();
    target
        .configure_workflow_learning(serde_json::to_string(&target_config).unwrap())
        .await
        .unwrap();
    let key = plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string();
    target.configure_training(serde_json::json!({"subject":"fixture-attestor", "credential":"fixture-key", "idempotency_key": key,
        "grant":{"grantee":"fixture-attestor", "credential":"fixture-key", "actions":["attest_imported_review"],
            "expires_at":"2099-01-01T00:00:00Z", "publication_policy":{"policy":"human_reviewed"}, "target":{"scope":"project"}}}).to_string()).await.unwrap();
    target
        .load_catalog(
            fixture
                .join("matrix.manifest.json")
                .to_string_lossy()
                .into_owned(),
        )
        .await
        .unwrap();
    target
        .activate_discovery(
            format!("import-proof-{}", producer.session_id),
            serde_json::to_string(&plasm_core::prerequisites::DeploymentBindings::default())
                .unwrap(),
        )
        .await
        .unwrap();
    target.authorize_imported_workflow_review(serde_json::json!({"archive_digest":manifest.digest, "evidence":evidence,
        "decision_id":plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string(), "expires_at":"2099-01-01T00:00:00Z"}).to_string()).await.unwrap();
    let policy = serde_json::json!({"expected_digest": manifest.digest, "source_scope": manifest.source_scope}).to_string();
    let imported: serde_json::Value = serde_json::from_str(
        &target
            .import_workflow_archive(archive_path.to_string_lossy().into_owned(), policy.clone())
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        imported["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["disposition"]["status"] == "eligible_candidate"),
        "{imported}"
    );
    let repeated: serde_json::Value = serde_json::from_str(
        &target
            .import_workflow_archive(archive_path.to_string_lossy().into_owned(), policy)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        repeated["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["disposition"]["status"] == "duplicate"),
        "{repeated}"
    );
    let checked: serde_json::Value =
        serde_json::from_str(&target.verify_workflow_library(1).await.unwrap()).unwrap();
    assert_eq!(checked["examples"][0]["outcome"]["status"], "checked");
    target.close().await;
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
