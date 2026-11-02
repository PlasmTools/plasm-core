use super::*;
fn collection() -> CollectionIdentity {
    CollectionIdentity {
        protocol_digest: ArchiveDigest::try_from("a".repeat(64)).unwrap(),
        tasks: vec![TaskMembership {
            task_id: "task".into(),
            group_id: "group".into(),
        }],
    }
}
fn assessment() -> TaskAssessment {
    TaskAssessment {
        attempt_id: "attempt".into(),
        task_id: "task".into(),
        group_id: "group".into(),
        logical_sessions: vec![],
        protocol_digest: collection().protocol_digest,
        assessor: "fixture-native-grade-v1".into(),
        outcome: TaskOutcome::Passed {
            passed: 3,
            total: 3,
        },
    }
}
#[test]
fn archive_integrity_limits_and_explicit_assessment_absence() {
    let scope = LibraryScope {
        tenant: "source".into(),
        project: "project".into(),
    };
    let item = ArchiveItem::new(
        "assessment:attempt".into(),
        ArchiveRecord::Assessment {
            assessment: assessment(),
        },
    )
    .unwrap();
    let manifest = ArchiveManifest {
        version: ARCHIVE_VERSION,
        source_scope: scope,
        kind: ArchiveKind::PrivateCollection,
        collection: collection(),
        assessment_coverage: AssessmentCoverage::NotIncluded,
        task_provenance: TaskProvenance::Complete,
        external_evidence: vec![],
    };
    let archive = ValidatedArchive::create(manifest.clone(), vec![item.clone()]).unwrap();
    let bytes = archive.encode().unwrap();
    let restored = ValidatedArchive::decode(&bytes).unwrap();
    assert_eq!(restored.encode().unwrap(), bytes);
    let mut value = serde_json::to_value(archive.document()).unwrap();
    value["items"][0]["record"]["assessment"]["outcome"]["total"] = 4.into();
    assert!(matches!(
        ValidatedArchive::decode(&serde_json::to_vec(&value).unwrap()),
        Err(ArchiveError::Integrity)
    ));
    assert!(matches!(
        ValidatedArchive::create(manifest.clone(), vec![item.clone(), item.clone()]),
        Err(ArchiveError::Reference)
    ));
    let mut serving = manifest;
    serving.kind = ArchiveKind::ServingLibrary;
    assert!(matches!(
        ValidatedArchive::create(serving, vec![item]),
        Err(ArchiveError::PrivateEvidence)
    ));
    let mut missing = assessment();
    missing.outcome = TaskOutcome::Unavailable {
        reason: AssessmentUnavailable::NotAttempted,
    };
    missing.validate().unwrap();
    let mut bad = assessment();
    bad.outcome = TaskOutcome::Passed {
        passed: 0,
        total: 3,
    };
    assert!(bad.validate().is_err());
}

#[tokio::test]
async fn postgres_archive_scored_retention_and_authorized_adoption(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::workflow_learning::{
        model::*, repository::WorkflowRepository, test_identities as ids,
    };
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        eprintln!("archive integration requires PLASM_TEST_POSTGRES_URL");
        return Ok(());
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let schema = format!("archive_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await?;
    let separator = if url.contains('?') { '&' } else { '?' };
    let repo =
        WorkflowRepository::connect(&format!("{url}{separator}options=-csearch_path%3D{schema}"))
            .await?;
    repo.migrate().await?;
    let original = crate::workflow_learning::teaching::tests::session(&["LangItem"]);
    let source = crate::workflow_learning::teaching::tests::source(&original);
    let compiled = crate::plasm_dag::compile_python_program_checked(&original, &source)?;
    let scope = LibraryScope {
        tenant: crate::workflow_learning::learning_tenant(&original.tenant_scope),
        project: "source".into(),
    };
    let target = LibraryScope {
        tenant: "target".into(),
        project: "evaluation".into(),
    };
    let logical = ids::logical("archive-source");
    let receipt=ExecutionReceipt{scope:scope.clone(),logical_session:logical,session_revision:1,run_id:ids::run("archive-run"),plan_id:CompiledProgramId::from_comp(&compiled.artifact().comp),prompt_hash:original.prompt_hash.parse()?,execute_session:ids::execute("archive-execute"),run_artifact:serde_json::json!({"complete":true}),source:source.clone(),compiled_plan:serde_json::to_value(&compiled.artifact().comp)?,observation:crate::workflow_learning::teaching::ProgramObservation::from_execution(&original,&source,&compiled),catalogue_requirements:crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::from_compiled(&original,&compiled),symbol_context:crate::program_symbol_context::SavedProgramContext::from_compilation(&compiled,&scope,&logical,ids::execute("archive-execute").as_str()),catalog_fingerprints:compiled.compilation_symbols().captured().unwrap().catalog_fingerprints().clone()};
    repo.bind_session(&scope, logical).await?;
    repo.begin_execution(&scope, logical, "run").await?;
    repo.record_execution(&receipt).await?;
    repo.finish_execution(&scope, logical, "run", true).await?;
    let mut grade = assessment();
    grade.logical_sessions.push(logical);
    repo.record_archive_assessment(&scope, &grade).await?;
    let request = ArchiveExportRequest {
        kind: ArchiveKind::PrivateCollection,
        collection: collection(),
    };
    let unreviewed = repo.export_archive(&scope, &request).await?;
    assert!(unreviewed
        .document()
        .items
        .iter()
        .any(|item| matches!(item.record, ArchiveRecord::Observation { .. })));
    assert!(
        repo.list_eligible(&scope, 10).await?.is_empty(),
        "task grade grants no program publication"
    );
    repo.review(&ReviewRequest {
        scope: scope.clone(),
        logical_session: logical,
        session_revision: 1,
        intent: "inspect records".into(),
        subtasks: vec![AchievedSubtask {
            intent: "inspect records".into(),
            observed_outcome: "observed fixture record".into(),
            run_ids: vec![receipt.run_id],
        }],
    })
    .await?;
    let claim = repo.claim_scoped(60, Some(&scope)).await?.unwrap();
    let checked = crate::workflow_learning::teaching::CheckedTeachingExample::from_observation(
        &receipt.observation,
    )?;
    let candidate = Candidate {
        intent: crate::workflow_learning::index::NormalizedIntent {
            purpose: "inspect records".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "fixture".into(),
        family: "inspect".into(),
        variant: "fixture".into(),
        identity: checked.program().structural_fingerprint.clone(),
        coverage: vec!["records".into()],
        quality: Quality::observed(&claim.seal, &[receipt.run_id], 1)?,
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(checked, claim.seal.assurance),
        },
        catalog_fingerprints: receipt.catalog_fingerprints.clone(),
        expected_family_version: 0,
    };
    repo.finish(&claim, &[candidate], PublicationPolicy::Automatic)
        .await?;
    let mut omitted = request.clone();
    omitted.collection.tasks.clear();
    assert!(matches!(
        repo.export_archive(&scope, &omitted).await,
        Err(ArchiveError::InvalidMetadata)
    ));
    let archive = repo.export_archive(&scope, &request).await?;
    let bytes = archive.encode()?;
    let policy = ArchiveImportPolicy {
        expected_digest: archive.document().digest.clone(),
        source_scope: scope.clone(),
        excluded_tasks: vec![],
        excluded_groups: vec![],
    };
    let host = crate::test_support::operation_fixtures::minimal_host();
    let mut destination = crate::workflow_learning::teaching::tests::session(&["LangLine"]);
    destination.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &destination.cgs,
        "langmatrix",
        &[],
    ));
    let allowed = crate::discovery_store::DiscoveryAuthorization {
        catalogs: ["langmatrix".into()].into(),
        capabilities: [("langmatrix".into(), ["langitem_query".into()].into())].into(),
    };
    // A valid source seal is retained even when an older checked witness cannot
    // be served. Diagnose that incompatibility rather than missing evidence.
    let mut old_items = archive.document().items.clone();
    for item in &mut old_items {
        if let ArchiveRecord::Example { candidate, .. } = &mut item.record {
            let mut old = serde_json::to_value(&*candidate)?;
            old["state"]["evidence"]["program"]["program"]["schema_version"] = 2.into();
            *candidate = serde_json::from_value(old)?;
            *item = ArchiveItem::new(item.identity.clone(), item.record.clone())?;
        }
    }
    let old_archive = ValidatedArchive::create(archive.document().manifest.clone(), old_items)?;
    let old_policy = ArchiveImportPolicy {
        expected_digest: old_archive.document().digest.clone(),
        ..policy.clone()
    };
    let old_scope = LibraryScope {
        tenant: "old-witness".into(),
        project: target.project.clone(),
    };
    let old_result = repo
        .import_archive(
            &old_scope,
            &old_archive.encode()?,
            &old_policy,
            &host,
            &destination,
            &allowed,
        )
        .await?;
    assert!(old_result.items.iter().any(|item| matches!(
        item.disposition,
        ImportDisposition::Quarantined {
            reason: ArchiveCompatibilityIssue::IncompatibleTeachingWitness {
                schema_version: 2,
                ..
            }
        }
    )));
    assert!(repo.list_eligible(&old_scope, 10).await?.is_empty());
    let mut unmapped_manifest = archive.document().manifest.clone();
    unmapped_manifest.task_provenance = TaskProvenance::Unmapped {
        logical_sessions: vec![logical],
        external_reviews: vec![],
    };
    let unmapped = ValidatedArchive::create(unmapped_manifest, archive.document().items.clone())?;
    let unmapped_policy = ArchiveImportPolicy {
        expected_digest: unmapped.document().digest.clone(),
        ..policy.clone()
    };
    let unknown_scope = LibraryScope {
        tenant: "unknown".into(),
        project: target.project.clone(),
    };
    let unknown = repo
        .import_archive(
            &unknown_scope,
            &unmapped.encode()?,
            &unmapped_policy,
            &host,
            &destination,
            &allowed,
        )
        .await?;
    assert!(unknown.items.iter().any(|item| matches!(
        item.disposition,
        ImportDisposition::AwaitingCompatibility {
            reason: ArchiveCompatibilityIssue::TaskProvenanceUnavailable
        }
    )));
    assert!(repo.list_eligible(&unknown_scope, 10).await?.is_empty());
    // Two earlier wrappers retain the same seal without complete attribution.
    // A later complete wrapper must be found regardless of physical row order.
    for extra in ["incomplete-a", "incomplete-b"] {
        let mut manifest = unmapped.document().manifest.clone();
        manifest.collection.tasks.push(TaskMembership {
            task_id: extra.into(),
            group_id: "extra-group".into(),
        });
        let wrapper = ValidatedArchive::create(manifest, archive.document().items.clone())?;
        let wrapper_policy = ArchiveImportPolicy {
            expected_digest: wrapper.document().digest.clone(),
            ..policy.clone()
        };
        repo.import_archive(
            &target,
            &wrapper.encode()?,
            &wrapper_policy,
            &host,
            &destination,
            &allowed,
        )
        .await?;
    }
    assert!(repo.list_eligible(&target, 10).await?.is_empty());
    let imported = repo
        .import_archive(&target, &bytes, &policy, &host, &destination, &allowed)
        .await?;
    assert!(
        imported
            .items
            .iter()
            .any(|item| matches!(item.disposition, ImportDisposition::EligibleCandidate)),
        "{imported:?}"
    );
    assert_eq!(repo.list_eligible(&target, 10).await?.len(), 1);
    let again = repo
        .import_archive(&target, &bytes, &policy, &host, &destination, &allowed)
        .await?;
    assert!(again
        .items
        .iter()
        .all(|item| matches!(item.disposition, ImportDisposition::Duplicate)));
    assert_eq!(
        repo.export_retained_archive(&target, &policy.expected_digest)
            .await?
            .encode()?,
        bytes
    );
    let reexport = repo
        .export_archive(
            &target,
            &ArchiveExportRequest {
                kind: ArchiveKind::ServingLibrary,
                collection: collection(),
            },
        )
        .await?;
    assert_eq!(
        reexport.document().manifest.task_provenance,
        TaskProvenance::Complete
    );
    assert!(reexport
        .document()
        .manifest
        .external_evidence
        .iter()
        .all(|reference| reference.source_scope == scope));
    assert!(!reexport.document().manifest.external_evidence.is_empty());
    // Query using the isolated schema, not the admin connection search_path.
    let counts:(i64,i64)=sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {schema}.workflow_learning_receipts WHERE tenant=$1),(SELECT count(*) FROM {schema}.workflow_learning_reviews WHERE tenant=$1)")).bind(&target.tenant).fetch_one(&admin).await?;
    assert_eq!(counts, (0, 0));
    let mut denied = policy.clone();
    denied.source_scope = target.clone();
    assert!(matches!(
        repo.import_archive(&target, &bytes, &denied, &host, &destination, &allowed)
            .await,
        Err(ArchiveError::Scope)
    ));
    denied = policy;
    denied.excluded_tasks.push("task".into());
    assert!(matches!(
        repo.import_archive(&target, &bytes, &denied, &host, &destination, &allowed)
            .await,
        Err(ArchiveError::InvalidMetadata)
    ));
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await?;
    Ok(())
}
