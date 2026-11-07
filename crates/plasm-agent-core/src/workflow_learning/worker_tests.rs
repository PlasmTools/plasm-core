//! Real compiler, PostgreSQL ledger and session publication with a scripted model.
use super::*;

#[tokio::test]
async fn a_failed_assessment_packet_does_not_discard_another_packet() {
    struct PartialModel;
    impl ProposalModel for PartialModel {
        fn embedding_profile(&self) -> String {
            "fixture".into()
        }
        fn normalize<'a>(
            &'a self,
            _: &'a NormalizationRequest,
        ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
            Box::pin(async { unreachable!() })
        }
        fn embed<'a>(
            &'a self,
            _: &'a NormalizedIntent,
        ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
            Box::pin(async { unreachable!() })
        }
        fn assess<'a>(
            &'a self,
            request: &'a super::super::applicability::Request,
        ) -> futures_util::future::LocalBoxFuture<
            'a,
            Result<BTreeMap<String, super::super::applicability::Assessment>, LearningError>,
        > {
            Box::pin(async move {
                if request.labels().contains("example_1") {
                    return Err(LearningError::ProviderConfiguration(
                        "fixture failure".into(),
                    ));
                }
                Ok(request
                    .labels()
                    .iter()
                    .map(|label| {
                        (
                            label.clone(),
                            super::super::applicability::Assessment {
                                judgment: super::super::applicability::Judgment::RequiredSubtask,
                                effect_alignment:
                                    super::super::applicability::EffectAlignment::Aligned,
                                constraint_alignment:
                                    super::super::applicability::ConstraintAlignment::Compatible,
                            },
                        )
                    })
                    .collect())
            })
        }
    }
    use super::super::applicability::{Candidate, CandidateKind, Request};
    let candidates: Vec<_> = (1..=6)
        .map(|i| Candidate {
            label: format!("example_{i}"),
            kind: CandidateKind::Example,
            purpose: "Inspect records".into(),
            dependencies: vec![],
            symbols: vec![],
            outputs: vec![],
            effects: vec![],
            source: Some("x".repeat(4_000)),
        })
        .collect();
    let requests = Request::batches("fixture", "Inspect records", &candidates).unwrap();
    assert!(requests.len() > 1);
    let (judgments, unavailable) = assess_packets(
        &PartialModel,
        &RecallBudget::new(Duration::from_secs(5)),
        &requests,
    )
    .await;
    assert!(unavailable.contains("example_1"));
    assert!(judgments.contains_key("example_6"));
    assert!(!judgments.contains_key("example_1"));
}

#[tokio::test]
async fn recall_budget_allows_sequential_stages_and_reports_the_expired_stage() {
    let budget = RecallBudget::new(Duration::from_millis(250));
    for stage in [RecallStage::Normalization, RecallStage::Embedding] {
        budget
            .run(
                stage,
                Duration::from_millis(100),
                tokio::time::sleep(Duration::from_millis(20)),
            )
            .await
            .unwrap();
    }
    let failure = budget
        .run(
            RecallStage::Applicability,
            Duration::from_millis(10),
            tokio::time::sleep(Duration::from_millis(40)),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        LearningError::RecallTimeout(RecallStage::Applicability)
    ));

    let exhausted = RecallBudget::new(Duration::from_millis(10));
    let failure = exhausted
        .run(
            RecallStage::Search,
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_millis(40)),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        LearningError::RecallTimeout(RecallStage::Search)
    ));
}
use crate::workflow_learning::{model::CompiledProgramId, test_identities as ids};

/// The producer and consumer are separate real Node/NAPI processes. The only
/// scripted component is the semantic proposal provider inside the normal worker.
#[tokio::test]
async fn postgres_native_process_restart_promotes_and_fetches_checked_example() {
    let (Ok(url), Ok(binding), Ok(manifest)) = (
        std::env::var("PLASM_TEST_POSTGRES_URL"),
        std::env::var("PLASM_NATIVE_BINDING"),
        std::env::var("PLASM_NATIVE_LEARNING_FIXTURE_MANIFEST"),
    ) else {
        eprintln!(
            "native workflow proof requires PostgreSQL, rebuilt binding and exported fixture"
        );
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("native_workflow_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let raw_scope = serde_json::json!({"tenant":"native-proof","project":schema});
    let scope = LibraryScope {
        tenant: "tenant:native-proof".into(),
        project: schema.clone(),
    };
    let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plasm-node/tests/workflow-learning-native.mjs");
    let run = |phase: &str, input: serde_json::Value| {
        use std::io::Write;
        let mut child = std::process::Command::new("node")
            .arg(&driver)
            .arg(phase)
            .env("PLASM_TEST_POSTGRES_URL", &scoped_url)
            .env("PLASM_NATIVE_BINDING", &binding)
            .env("PLASM_NATIVE_LEARNING_FIXTURE_MANIFEST", &manifest)
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("AI_GATEWAY_API_KEY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&input).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "native {phase} exceeded90seconds: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "native {phase} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let collected = run("collect", serde_json::json!({"scope":raw_scope}));
    let repo = WorkflowRepository::connect(&scoped_url).await.unwrap();
    let claim = repo.claim_scoped(300, Some(&scope)).await.unwrap().unwrap();
    assert_eq!(
        collected["review_ref"].as_str().unwrap(),
        claim.id.to_string()
    );
    for receipt in &claim.seal.executions {
        assert!(receipt.observation.available().is_some());
        assert!(receipt.catalog_fingerprints.contains_key("matrix"));
    }
    let CurationBatch {
        examples: proposals,
        associations,
    } = candidates(&repo, &claim, &OfflineModel).await.unwrap();
    assert_eq!(collected["sources"].as_array().unwrap().len(), 2);
    assert!(collected["groups"][0].as_array().unwrap().len() >= 2);
    assert!(claim.seal.executions.len() >= 3);
    let authored: BTreeSet<_> = claim
        .seal
        .executions
        .iter()
        .map(|receipt| receipt.source.as_str())
        .collect();
    assert_eq!(
        authored.len(),
        2,
        "receipt multiplicity must not manufacture program units"
    );
    assert_eq!(
        proposals.len(),
        2,
        "two whole programs, not one example per returned artifact"
    );
    for proposal in &proposals {
        assert!(proposal.verified_by(&claim.seal));
        let program = proposal.program().unwrap();
        assert!(!program.required_capabilities.is_empty());
        assert!(program
            .required_capabilities
            .iter()
            .all(|cap| cap.catalog == "matrix"));
        assert_eq!(
            program
                .catalog_fingerprints
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["matrix"]
        );
    }
    repo.finish_with_guidance(&claim, &proposals, &associations, PublicationPolicy::Shadow)
        .await
        .unwrap();
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    let artifacts = repo.export_proposals(&scope).await.unwrap();
    let hashes = artifacts
        .iter()
        .map(|item| item.candidate_hash.clone())
        .collect::<Vec<_>>();
    repo.publish_training_fixture(&scope, claim.id, &hashes, "offline-native-proof")
        .await
        .unwrap();
    let examples = repo.list_eligible(&scope, 3).await.unwrap();
    assert_eq!(examples.len(), 2);
    let mut kinds = BTreeSet::new();
    for example in &examples {
        let program = example.candidate.program().unwrap();
        let kind = if program.source.contains("class InspectProducts") {
            "products"
        } else {
            assert!(program.source.contains("class InspectNames"));
            "names"
        };
        kinds.insert(kind);
        let fetched = run(
            "fetch",
            serde_json::json!({"scope":raw_scope,"example_ref":example.id,"program_kind":kind}),
        );
        assert_eq!(fetched["compiled"], true);
        assert!(fetched["source"].as_str().unwrap().contains("e2."));
        assert_eq!(fetched["catalogs"], serde_json::json!(["matrix"]));
        assert_eq!(fetched["outputs"], if kind == "products" { 2 } else { 1 });
    }
    assert_eq!(kinds, BTreeSet::from(["products", "names"]));
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
use std::sync::Arc;

fn fixture_exchange(proposal: serde_json::Value) -> NormalizationExchange {
    let raw=serde_json::json!({"choices":[{"message":{"content":proposal.to_string()}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}).to_string();
    let mut capture = NormalizationCapture::default();
    capture.response(200, &raw);
    NormalizationExchange {
        response: Ok(raw),
        capture,
    }
}

struct RejectedModel<'a> {
    repo: &'a WorkflowRepository,
    scope: &'a LibraryScope,
}
impl ProposalModel for RejectedModel<'_> {
    fn embedding_profile(&self) -> String {
        panic!("rejected normalization must not embed")
    }
    fn normalize<'a>(
        &'a self,
        _: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        Box::pin(async move {
            let audits = self
                .repo
                .normalization_audits(self.scope, 100)
                .await
                .unwrap();
            assert!(
                audits.iter().any(|audit| audit["state"] == "pending"),
                "dispatch requires a committed pending audit"
            );
            fixture_exchange(
                serde_json::json!({"intent":{"purpose":" ","facets":[],"aliases":[]},"family":{"kind":"new"}}),
            )
        })
    }
    fn embed<'a>(
        &'a self,
        _: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        panic!("rejected normalization must not embed")
    }
}

struct OfflineModel;
impl ProposalModel for OfflineModel {
    fn assess<'a>(
        &'a self,
        request: &'a super::super::applicability::Request,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<BTreeMap<String, super::super::applicability::Assessment>, LearningError>,
    > {
        Box::pin(async move {
            Ok(request
                .labels()
                .iter()
                .cloned()
                .map(|label| {
                    (
                        label,
                        super::super::applicability::Assessment {
                            judgment: super::super::applicability::Judgment::SameTask,
                            effect_alignment: super::super::applicability::EffectAlignment::Aligned,
                            constraint_alignment:
                                super::super::applicability::ConstraintAlignment::Compatible,
                        },
                    )
                })
                .collect())
        })
    }
    fn embedding_profile(&self) -> String {
        "offline-matrix-3-v1".into()
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        Box::pin(async move {
            let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
            let input: serde_json::Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let family = input["existing"]
                .as_array()
                .unwrap()
                .first()
                .map(|example| serde_json::json!({"kind":"existing","family":example["family"]}))
                .unwrap_or_else(|| serde_json::json!({"kind":"new"}));
            fixture_exchange(
                serde_json::json!({"intent":{"purpose":"inspect an item","facets":["read only"],"aliases":[]},"family":family}),
            )
        })
    }
    fn embed<'a>(
        &'a self,
        _: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        Box::pin(async { Ok(vec![1.0, 0.0, 0.0]) })
    }
}

struct UnrelatedModel;

struct SlowModel;
impl ProposalModel for SlowModel {
    fn embedding_profile(&self) -> String {
        OfflineModel.embedding_profile()
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            OfflineModel.normalize(request).await
        })
    }
    fn embed<'a>(
        &'a self,
        intent: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        OfflineModel.embed(intent)
    }
    fn assess<'a>(
        &'a self,
        request: &'a super::super::applicability::Request,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<BTreeMap<String, super::super::applicability::Assessment>, LearningError>,
    > {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            OfflineModel.assess(request).await
        })
    }
}

impl ProposalModel for UnrelatedModel {
    fn embedding_profile(&self) -> String {
        OfflineModel.embedding_profile()
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        OfflineModel.normalize(request)
    }
    fn embed<'a>(
        &'a self,
        intent: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        OfflineModel.embed(intent)
    }
    fn assess<'a>(
        &'a self,
        request: &'a super::super::applicability::Request,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<BTreeMap<String, super::super::applicability::Assessment>, LearningError>,
    > {
        Box::pin(async move {
            Ok(request
                .labels()
                .iter()
                .cloned()
                .map(|label| {
                    (
                        label,
                        super::super::applicability::Assessment {
                            judgment: super::super::applicability::Judgment::Unrelated,
                            effect_alignment:
                                super::super::applicability::EffectAlignment::Unrequested,
                            constraint_alignment:
                                super::super::applicability::ConstraintAlignment::Compatible,
                        },
                    )
                })
                .collect())
        })
    }
}

struct ForbiddenModel;
impl ProposalModel for ForbiddenModel {
    fn embedding_profile(&self) -> String {
        panic!("frozen replay must not consult model configuration")
    }
    fn normalization_protocol(&self) -> Result<NormalizationChat, NormalizationError> {
        panic!("ineligible/frozen work must not configure normalization")
    }
    fn normalize<'a>(
        &'a self,
        _: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        panic!("ineligible/frozen work must not normalize")
    }
    fn embed<'a>(
        &'a self,
        _: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        panic!("frozen replay must not embed again")
    }
}

#[tokio::test]
async fn postgres_review_to_checked_example_without_provider_calls() {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        eprintln!("PLASM_TEST_POSTGRES_URL absent: workflow integration not exercised");
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("learning_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let repo = WorkflowRepository::connect(&scoped_url).await.unwrap();
    repo.migrate().await.unwrap();
    let session = super::super::teaching::tests::session(&["LangItem", "LangLine"]);
    let source = super::super::teaching::tests::source(&session).replace(
        ".query()",
        ".query().where(lambda row: row.title == \"unannotated-label\")",
    );
    let expression = source
        .lines()
        .last()
        .unwrap()
        .trim()
        .strip_prefix("return ")
        .unwrap();
    let source = source.replace(
        &format!("return {expression}"),
        &format!(
            "return {expression}, {}",
            expression.replace("take(1)", "take(2)")
        ),
    );

    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let teaching = TeachingProgram::from_compiled(&session, &source, &bundle).unwrap();
    let scope = LibraryScope {
        tenant: "local-appliance".into(),
        project: "matrix".into(),
    };
    repo.bind_training_fixture(&scope, ids::logical("logical"))
        .await
        .unwrap();
    repo.begin_execution(&scope, ids::logical("logical"), "attempt")
        .await
        .unwrap();
    // This is the trusted execution-host boundary: domain execution itself is
    // represented by a fixture receipt, never by an LLM success assertion.
    let receipt = ExecutionReceipt {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        execution_id: ids::run("fixture-run"),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("fixture-execute"),
        source: source.clone(),
        teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
            reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },

        catalogue_requirements:
            super::super::catalogue_guidance::CatalogueRequirementEvidence::Available(
                super::super::teaching::ProgramCatalogueRequirements::from_compiled(
                    &session, &bundle,
                )
                .unwrap(),
            ),
        compiled_plan: serde_json::to_value(&bundle.artifact().comp).unwrap(),
        observation: super::super::teaching::ProgramObservation::from_execution(
            &session, &source, &bundle,
        ),
        catalog_fingerprints: teaching.catalog_fingerprints.clone(),
    };
    repo.record_training_fixture(&receipt).await.unwrap();
    let mut other_output = receipt.clone();
    other_output.execution_id = ids::run("fixture-other-output");
    repo.record_training_fixture(&other_output).await.unwrap();
    let request = ReviewRequest {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        intent: "Inspect one item".into(),
        subtasks: vec![
            AchievedSubtask {
                intent: "Inspect one item".into(),
                observed_outcome: "One complete item was returned".into(),
                execution_ids: vec![ids::run("fixture-run")],
            },
            AchievedSubtask {
                intent: "Inspect another output".into(),
                observed_outcome: "The second result completed".into(),
                execution_ids: vec![ids::run("fixture-other-output")],
            },
        ],
    };
    assert!(repo.review_training_fixture(&request).await.is_err());
    repo.finish_execution(&scope, ids::logical("logical"), "attempt", true)
        .await
        .unwrap();
    let review = repo.review_training_fixture(&request).await.unwrap();
    assert!(
        repo.claim_recorded_scoped(60, Some(&scope))
            .await
            .unwrap()
            .is_none(),
        "missing provider must leave fresh proposals unclaimed"
    );
    let claim = repo.claim_scoped(60, Some(&scope)).await.unwrap().unwrap();
    assert_eq!(claim.id, review.id);
    assert!(NormalizationContext::review(&claim, usize::MAX, NormalizationStage::Initial).is_err());
    assert!(NormalizationContext::review(
        &claim,
        0,
        NormalizationStage::Compare {
            execution_id: ids::run("not-in-seal")
        }
    )
    .is_err());
    let mut unobserved = claim.clone();
    for execution in &mut unobserved.seal.executions {
        execution.observation = super::super::teaching::ProgramObservation::Unavailable(
            super::super::teaching::ObservationDiagnostic::Extraction {
                message: "fixture unsupported program remains in immutable receipt".into(),
            },
        );
        execution.catalogue_requirements = crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() };
    }
    assert!(candidates(&repo, &unobserved, &ForbiddenModel)
        .await
        .unwrap()
        .examples
        .is_empty());
    assert!(repo
        .normalization_audits(&scope, 100)
        .await
        .unwrap()
        .is_empty());
    let rejected = interpret(
        &repo,
        NormalizationContext::review(&claim, 0, NormalizationStage::Initial).unwrap(),
        &RejectedModel {
            repo: &repo,
            scope: &scope,
        },
        "inspect",
        InterpretationMaterial::default(),
        None,
    )
    .await;
    assert!(matches!(
        rejected,
        Err(LearningError::Normalization(
            NormalizationError::BlankText { .. }
        ))
    ));
    let audit = repo.normalization_audits(&scope, 100).await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0]["state"], "rejected");
    assert_eq!(
        audit[0]["result"]["result"]["diagnostic"]["predicate"],
        "blank_text"
    );
    assert_eq!(
        audit[0]["result"]["capture"]["response"]["usage"]["status"],
        "reported"
    );

    let CurationBatch {
        examples: proposals,
        associations,
    } = candidates(&repo, &claim, &OfflineModel).await.unwrap();
    assert_eq!(proposals.len(), 1);
    let audits = repo.normalization_audits(&scope, 100).await.unwrap();
    assert_eq!(
        audits.len(),
        3,
        "one rejection then one initial/compare pair per complete program"
    );
    assert_eq!(proposals[0].program().unwrap().outputs.len(), 2);
    assert!(
        proposals[0]
            .program()
            .unwrap()
            .source
            .contains("unannotated-label"),
        "curation retains ordinary unannotated authored literals"
    );
    assert!(proposals[0].eligible(OutcomeAssurance::AgentAttested));
    repo.finish_with_guidance(
        &claim,
        &proposals,
        &associations,
        PublicationPolicy::Reviewed,
    )
    .await
    .unwrap();
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    let original_artifacts = repo.export_proposals(&scope).await.unwrap();
    repo.approve_training_fixture(&scope, review.id, "fixture-operator")
        .await
        .unwrap();
    assert!(repo
        .claim_recorded_scoped(60, Some(&scope))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        serde_json::to_value(&original_artifacts).unwrap(),
        serde_json::to_value(repo.export_proposals(&scope).await.unwrap()).unwrap()
    );
    let catalogs = BTreeMap::from([(
        "langmatrix".to_owned(),
        plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap(),
    )]);
    let (examples, guidance) = recall_with_model(
        &repo,
        &scope,
        "Inspect one item",
        &crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from([
            "langmatrix".into()
        ])),
        &RecallCatalogue {
            pins: &receipt.catalog_fingerprints,
            graphs: &catalogs,
        },
        &OfflineModel,
        &RecallBudget::new(Duration::from_secs(30)),
    )
    .await
    .unwrap();
    assert_eq!(examples.len(), 1);
    assert!(!guidance.is_empty());
    let mut altered = examples.clone();
    altered[0].candidate.intent.purpose.push_str(" changed");
    let checked = super::super::deferred_recall::validate_current(
        &WorkflowLearning::with_test_repository(
            LearningMode::Serving,
            std::sync::Arc::new(repo.clone()),
        ),
        &scope,
        (altered, guidance.clone()),
    )
    .await
    .unwrap();
    assert!(
        checked.0.is_empty(),
        "changed example revision must not be offered"
    );
    assert!(!checked.1.is_empty());
    let (slow_examples, slow_guidance) = recall_with_model(
        &repo,
        &scope,
        "Inspect one item",
        &crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from([
            "langmatrix".into()
        ])),
        &RecallCatalogue {
            pins: &receipt.catalog_fingerprints,
            graphs: &catalogs,
        },
        &SlowModel,
        &RecallBudget::new(Duration::from_secs(30)),
    )
    .await
    .unwrap();
    assert_eq!(slow_examples.len(), 1);
    assert!(!slow_guidance.is_empty());
    let timed_out = interpret(
        &repo,
        NormalizationContext::recall(scope.clone()),
        &SlowModel,
        "inspect",
        InterpretationMaterial::default(),
        Some(Duration::from_millis(10)),
    )
    .await;
    assert!(matches!(
        timed_out,
        Err(LearningError::RecallTimeout(RecallStage::Normalization))
    ));
    assert!(repo
        .normalization_audits(&scope, 100)
        .await
        .unwrap()
        .iter()
        .any(|audit| audit["state"] == "transport_failed"));
    let (unrelated_examples, unrelated_guidance) = recall_with_model(
        &repo,
        &scope,
        "A different task",
        &crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from([
            "langmatrix".into()
        ])),
        &RecallCatalogue {
            pins: &receipt.catalog_fingerprints,
            graphs: &catalogs,
        },
        &UnrelatedModel,
        &RecallBudget::new(Duration::from_secs(30)),
    )
    .await
    .unwrap();
    assert!(unrelated_examples.is_empty());
    assert!(!unrelated_guidance.is_empty(), "a different whole-program purpose must not suppress authorized catalogue candidates before discovery judges their individual capabilities");
    // A different complete authored program in the same logical session is a
    // separate candidate, even when review intent uses the same vocabulary.
    let second_source = super::super::teaching::tests::source(&session);
    let second_bundle =
        crate::plasm_dag::compile_python_program_checked(&session, &second_source).unwrap();
    let mut second_program = receipt.clone();
    second_program.execution_id = ids::run("second-program-run");
    second_program.plan_id = CompiledProgramId::from_comp(&second_bundle.artifact().comp);
    second_program.session_revision = 2;
    second_program.source = second_source.clone();
    second_program.compiled_plan = serde_json::to_value(&second_bundle.artifact().comp).unwrap();
    second_program.observation = super::super::teaching::ProgramObservation::from_execution(
        &session,
        &second_source,
        &second_bundle,
    );
    second_program.catalogue_requirements =
        super::super::catalogue_guidance::CatalogueRequirementEvidence::Available(
            super::super::teaching::ProgramCatalogueRequirements::from_compiled(
                &session,
                &second_bundle,
            )
            .unwrap(),
        );
    repo.record_training_fixture(&second_program).await.unwrap();
    let mut next_request = request.clone();
    next_request.session_revision += 1;
    next_request.subtasks.push(AchievedSubtask {
        intent: "Inspect a single result in another complete program".into(),
        observed_outcome: "A complete single result was returned".into(),
        execution_ids: vec![second_program.execution_id],
    });
    repo.review_training_fixture(&next_request).await.unwrap();
    let next_claim = repo.claim_scoped(60, Some(&scope)).await.unwrap().unwrap();
    let conflicting = candidates(&repo, &next_claim, &ConflictingIntentModel)
        .await
        .unwrap();
    assert_eq!(conflicting.examples.len(), 2);
    assert!(matches!(
        conflicting.examples[0].state,
        CandidateState::Uncertain { .. }
    ));
    assert!(!conflicting.examples[0].eligible(OutcomeAssurance::AgentAttested));

    let registry = Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(vec![(
        "langmatrix".into(),
        "Matrix".into(),
        vec![],
        session.cgs.clone(),
    )]));
    let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
        engine: plasm_runtime::ExecutionEngine::new(Default::default()).unwrap(),
        mode: plasm_runtime::ExecutionMode::Live,
        registry,
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    let created = crate::http_execute::execute_session_create_response(
        &host,
        None,
        crate::http_execute::CreateExecuteSessionBody {
            entry_id: "langmatrix".into(),
            entities: vec!["LangLine".into(), "LangItem".into()],
            principal: None,
            logical_session_id: None,
            context_intent: None,
        },
    )
    .await
    .unwrap();
    let allowed = crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from([
        "langmatrix".into(),
    ]));
    // An example can provide the first declaration wave to a session whose
    // symbol ledger is empty. Build this fixture independently of the existing
    // nonempty session below, so both serving cases remain covered.
    let mut empty_session = (*host
        .get_execute_session(&created.prompt_hash, &created.session)
        .await
        .unwrap())
    .clone();
    empty_session.entities.clear();
    empty_session.python_teaching = Default::default();
    empty_session.domain_revision = 0;
    empty_session.teaching_exposure =
        Some(plasm_core::TeachingExposureSession::new_with_intent_delta(
            &empty_session.cgs,
            "langmatrix",
            &[],
            Default::default(),
        ));
    let empty_session_id = crate::execute_path_ids::ExecuteSessionId::new_random().to_string();
    let empty_key = crate::execute_session::SessionReuseKey {
        tenant_scope: String::new(),
        entry_id: "langmatrix".into(),
        catalog_cgs_hash: empty_session.cgs.catalog_cgs_hash_hex(),
        entities: vec![],
        context_intent: Some("Inspect a result".into()),
        principal: None,
        logical_session_id: Some(empty_session_id.clone()),
    };
    host.store_execute_session(
        empty_key,
        created.prompt_hash.clone(),
        empty_session_id.clone(),
        empty_session,
    )
    .await
    .unwrap();
    assert!(host
        .sessions
        .get_by_strs(&created.prompt_hash, &empty_session_id)
        .await
        .is_some());
    assert!(host
        .execute_session_registry
        .load_for_example(&created.prompt_hash, &empty_session_id)
        .await
        .unwrap()
        .is_some());
    let empty_prepared = super::super::serving::prepare_example_fetch(
        &host,
        &repo,
        scope.clone(),
        examples[0].id,
        &created.prompt_hash,
        &empty_session_id,
        &allowed,
    )
    .await
    .unwrap();
    let first_wave =
        super::super::serving::commit_example_fetch(&host, &repo, empty_prepared, &allowed)
            .await
            .unwrap();
    assert!(!first_wave.source.is_empty());
    assert!(
        !first_wave.teaching_delta.is_empty(),
        "the first fetch must teach its dependencies"
    );
    let prepared = super::super::serving::prepare_example_fetch(
        &host,
        &repo,
        scope.clone(),
        examples[0].id,
        &created.prompt_hash,
        &created.session,
        &allowed,
    )
    .await
    .unwrap();
    let served = super::super::serving::commit_example_fetch(&host, &repo, prepared, &allowed)
        .await
        .unwrap();
    assert!(!served.source.is_empty());
    assert!(!served.outputs.is_empty());
    let prepared = super::super::serving::prepare_example_fetch(
        &host,
        &repo,
        scope.clone(),
        examples[0].id,
        &created.prompt_hash,
        &created.session,
        &allowed,
    )
    .await
    .unwrap();
    let repeated = super::super::serving::commit_example_fetch(&host, &repo, prepared, &allowed)
        .await
        .unwrap();
    assert_eq!(served.source, repeated.source);
    assert_eq!(served.domain_revision, repeated.domain_revision);
    repo.quarantine(&scope, examples[0].id, "fixture drift")
        .await
        .unwrap();
    assert!(super::super::serving::prepare_example_fetch(
        &host,
        &repo,
        scope,
        examples[0].id,
        &created.prompt_hash,
        &created.session,
        &allowed
    )
    .await
    .is_err());
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}

fn candidate(identity: &str, tokens: u32, coverage: &[&str]) -> Candidate {
    let checked = super::super::teaching::tests::checked_fixture();
    let catalogs = checked.program().catalog_fingerprints.clone();
    Candidate {
        intent: NormalizedIntent {
            purpose: "inspect".into(),
            facets: vec![],
            aliases: vec![],
        },
        embedding: vec![1.0],
        embedding_profile: "test-profile".into(),
        family: "inspect".into(),
        variant: "matrix".into(),
        identity: identity.into(),
        coverage: coverage.iter().map(|s| s.to_string()).collect(),
        quality: Quality::unassessed(OutcomeAssurance::AgentAttested, tokens),
        catalog_fingerprints: catalogs,
        state: CandidateState::Verified {
            evidence: VerifiedProgram::observed(checked, OutcomeAssurance::AgentAttested),
        },
        expected_family_version: 0,
    }
}

#[test]
fn matching_purpose_and_coverage_do_not_prove_equivalent_behavior() {
    let old = candidate("old", 20, &["read"]);
    let stored = StoredExample {
        id: uuid::Uuid::nil(),
        candidate: old.clone(),
    };
    let mut intent = old.intent.clone();
    intent.purpose = "  INSPECT  ".into();
    assert!(ambiguous_new_family(
        std::slice::from_ref(&stored),
        &intent,
        &old.catalog_fingerprints,
        "different",
        &old.coverage
    ));
    assert!(!ambiguous_new_family(
        std::slice::from_ref(&stored),
        &intent,
        &old.catalog_fingerprints,
        "old",
        &old.coverage
    ));
    assert!(!ambiguous_new_family(
        std::slice::from_ref(&stored),
        &intent,
        &old.catalog_fingerprints,
        "different",
        &["write".into()]
    ));
    let mut other = old.catalog_fingerprints.clone();
    other.insert("other".into(), "different".into());
    assert!(!ambiguous_new_family(
        &[stored],
        &intent,
        &other,
        "different",
        &old.coverage
    ));
}

#[test]
fn selection_retains_best_and_complementary_contracts_independent_of_order() {
    let mut stronger = candidate("cheap", 20, &["read"]);
    stronger.quality.assurance = OutcomeAssurance::PostconditionVerified;
    stronger.state = CandidateState::Verified {
        evidence: VerifiedProgram::observed(
            super::super::teaching::tests::checked_fixture(),
            OutcomeAssurance::PostconditionVerified,
        ),
    };
    let proposals = [
        candidate("expensive", 100, &["read"]),
        stronger,
        candidate("write", 40, &["write"]),
    ];
    let mut forward = BTreeMap::new();
    let mut reverse = BTreeMap::new();
    for proposal in &proposals {
        select_candidate(&mut forward, proposal.clone());
    }
    for proposal in proposals.iter().rev() {
        select_candidate(&mut reverse, proposal.clone());
    }
    let identities = |map: BTreeMap<ProposalKey, Candidate>| {
        map.into_values().map(|c| c.identity).collect::<Vec<_>>()
    };
    assert_eq!(identities(forward), vec!["cheap", "write"]);
    assert_eq!(identities(reverse), vec!["cheap", "write"]);
}

#[test]
fn selection_prefers_verified_assurance_before_token_cost() {
    let mut map = BTreeMap::new();
    let mut robust = candidate("robust", 100, &["read"]);
    robust.quality.assurance = OutcomeAssurance::PostconditionVerified;
    robust.state = CandidateState::Verified {
        evidence: VerifiedProgram::observed(
            super::super::teaching::tests::checked_fixture(),
            OutcomeAssurance::PostconditionVerified,
        ),
    };
    let mut broken = candidate("broken", 1, &["read"]);
    broken.state = CandidateState::Proposed {
        diagnostic: ProposalDiagnostic {
            kind: DiagnosticKind::UnprovedRewrite,
            message: "unproved".into(),
        },
    };
    for proposal in [robust, candidate("cheap", 20, &["read"]), broken] {
        select_candidate(&mut map, proposal);
    }
    let eligible: Vec<_> = map
        .values()
        .filter(|candidate| candidate.eligible(OutcomeAssurance::PostconditionVerified))
        .map(|candidate| candidate.identity.as_str())
        .collect();
    assert_eq!(eligible, vec!["robust"]);
    let retained = map
        .values()
        .find(|candidate| candidate.identity == "broken")
        .expect("unproved diagnostic retained independently of verified behavior");
    assert!(matches!(retained.state, CandidateState::Proposed { .. }));
    assert!(retained.diagnostic().is_some());
    assert!(!retained.eligible(OutcomeAssurance::AgentAttested));
}

#[test]
fn provider_failures_retry_transient_transport_and_truncation() {
    use plasm_runtime::provider_transport::ProviderTransportError;
    let rejected = |status| {
        LearningError::Provider(ProviderTransportError::ProviderRejected {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            message: "fixture".into(),
            retry_after: None,
        })
    };
    assert!(retryable(&rejected(429)));
    assert!(retryable(&rejected(503)));
    assert!(retryable(&LearningError::Normalization(
        NormalizationError::Truncated
    )));
    assert!(!retryable(&rejected(401)));
    assert!(!retryable(&LearningError::ProviderResponse(
        "malformed proposal".into()
    )));
    assert!(!retryable(&LearningError::ProviderConfiguration(
        "missing key".into()
    )));
    assert!(retryable(&LearningError::Provider(
        ProviderTransportError::RetryExhausted {
            attempts: 3,
            source: Box::new(ProviderTransportError::DeadlineExpired)
        }
    )));
    assert!(retryable(&LearningError::Repository(
        super::super::repository::RepositoryError::Database(sqlx::Error::PoolTimedOut)
    )));
    assert!(retryable(&LearningError::Repository(
        super::super::repository::RepositoryError::Database(sqlx::Error::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        )))
    )));
    assert!(!retryable(&LearningError::Repository(
        super::super::repository::RepositoryError::InvalidEvidence("missing receipt")
    )));
}

#[test]
fn effect_contracts_are_pinned_and_missing_contracts_withhold_examples() {
    let session = super::super::teaching::tests::session(&["LangItem", "LangLine"]);
    let source = super::super::teaching::tests::source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let mut program = TeachingProgram::from_compiled(&session, &source, &bundle).unwrap();
    let mut cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let name = cgs.capabilities.keys().next().unwrap().clone();
    let capability = cgs.capabilities.get_mut(&name).unwrap();
    capability.kind = plasm_core::schema::CapabilityKind::Delete;
    capability.description = "Delete a record".into();
    program.required_capabilities = vec![plasm_core::prerequisites::CapabilityRef {
        catalog: "langmatrix".into(),
        capability: name.to_string(),
    }];
    let catalogs = BTreeMap::from([("langmatrix".to_owned(), cgs)]);
    let effects = effect_contracts(&program, &catalogs).unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].kind, plasm_core::schema::CapabilityKind::Delete);
    assert_eq!(effects[0].description, "Delete a record");
    program.required_capabilities[0].capability = "missing".into();
    assert!(effect_contracts(&program, &catalogs).is_none());
}

#[test]
fn coverage_ignores_node_ids_and_literals_but_retains_capability_contracts() {
    let session = super::super::teaching::tests::session(&["LangItem", "LangLine"]);
    let source = super::super::teaching::tests::source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let teaching = TeachingProgram::from_compiled(&session, &source, &bundle).unwrap();
    let mut renamed = teaching.clone();
    for output in &mut renamed.outputs {
        output.step = "different compiler id".into();
    }
    assert_eq!(coverage(&teaching).unwrap(), coverage(&renamed).unwrap());
    renamed.required_capabilities.clear();
    assert_ne!(coverage(&teaching).unwrap(), coverage(&renamed).unwrap());
}

#[test]
fn compiled_predicate_cannot_be_cost_displaced_by_guard_removal() {
    let session = super::super::teaching::tests::session(&["LangItem"]);
    let unguarded = super::super::teaching::tests::source(&session);
    let guarded = unguarded.replace(
        ".query()",
        ".query().where(lambda row: row.id == row.title)",
    );
    let compile = |source: &str| {
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, source).unwrap();
        TeachingProgram::from_compiled(&session, source, &bundle).unwrap()
    };
    let guarded = compile(&guarded);
    let unguarded = compile(&unguarded);
    assert_eq!(coverage(&guarded).unwrap(), coverage(&unguarded).unwrap());
    assert_ne!(
        guarded.behavior_fingerprint(),
        unguarded.behavior_fingerprint()
    );
    let mut expensive = candidate("guarded", 100, &[]);
    expensive.coverage = coverage(&guarded).unwrap();
    expensive.state = CandidateState::Verified {
        evidence: VerifiedProgram::observed(
            super::super::teaching::tests::checked_predicate_fixture(),
            OutcomeAssurance::AgentAttested,
        ),
    };
    let mut cheap = candidate("unguarded", 10, &[]);
    cheap.coverage = coverage(&unguarded).unwrap();

    let mut proposals = BTreeMap::new();
    select_candidate(&mut proposals, expensive);
    select_candidate(&mut proposals, cheap);
    // Distinct authored behavior remains separate regardless of teaching cost.
    assert_eq!(proposals.len(), 2);
    assert!(proposals.values().any(|p| p.identity == "guarded"));
}

struct ConflictingIntentModel;
impl ProposalModel for ConflictingIntentModel {
    fn embedding_profile(&self) -> String {
        "offline-matrix-3-v1".into()
    }
    fn normalize<'a>(
        &'a self,
        _: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        Box::pin(async {
            fixture_exchange(
                serde_json::json!({"intent":{"purpose":"inspect an item","facets":["conflicting outcome constraint"],"aliases":[]},"family":{"kind":"uncertain"}}),
            )
        })
    }
    fn embed<'a>(
        &'a self,
        _: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        Box::pin(async { Ok(vec![1.0, 0.0, 0.0]) })
    }
}

#[test]
fn tampered_durable_checked_source_cannot_become_eligible() {
    let original = candidate("integrity", 20, &["read"]);
    let mut wire = serde_json::to_value(&original).unwrap();
    wire["state"]["evidence"]["program"]["program"]["source"] = serde_json::json!(
        "class Forged(Program):\n    def build(self):\n        return e999.query()\n"
    );
    let restored: Candidate = serde_json::from_value(wire).unwrap();
    assert!(!restored.eligible(OutcomeAssurance::AgentAttested));
}

struct WithheldSourceModel;
impl ProposalModel for WithheldSourceModel {
    fn embedding_profile(&self) -> String {
        OfflineModel.embedding_profile()
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
        let input: serde_json::Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert!(
            input["observed_program"].is_object(),
            "worker reaches curation with retained observation"
        );
        assert!(
            input["observed_program"]["source"].is_null(),
            "known sensitive source is never sent to the model"
        );
        assert!(input["catalogue_definitions"].is_array());
        assert!(input.get("catalogue_requirements").is_none());
        assert!(!input.to_string().contains("observed_outcome"));
        assert!(!input.to_string().contains("run_artifact"));
        OfflineModel.normalize(request)
    }
    fn embed<'a>(
        &'a self,
        intent: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        OfflineModel.embed(intent)
    }
}

#[tokio::test]
async fn postgres_sensitive_source_still_reaches_curation_and_independent_guidance() {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("sensitive_curation_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let repo =
        WorkflowRepository::connect(&format!("{url}{separator}options=-csearch_path%3D{schema}"))
            .await
            .unwrap();
    repo.migrate().await.unwrap();
    let (session, source, bundle) = super::super::teaching::tests::sensitive_fixture();
    let scope = LibraryScope {
        tenant: "local-appliance".into(),
        project: schema.clone(),
    };
    let requirements =
        super::super::teaching::ProgramCatalogueRequirements::from_compiled(&session, &bundle)
            .unwrap();
    let observation =
        super::super::teaching::ProgramObservation::from_execution(&session, &source, &bundle);
    assert!(observation.available().unwrap().model_source().is_none());
    repo.bind_training_fixture(&scope, ids::logical("logical"))
        .await
        .unwrap();
    repo.begin_execution(&scope, ids::logical("logical"), "attempt")
        .await
        .unwrap();
    repo.record_training_fixture(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        execution_id: ids::run("sensitive-run"),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("execute"),
        source,
        compiled_plan: serde_json::to_value(&bundle.artifact().comp).unwrap(),
        observation,
        catalog_fingerprints: requirements.catalog_fingerprints().clone(),
        catalogue_requirements:
            super::super::catalogue_guidance::CatalogueRequirementEvidence::Available(requirements),
        teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
            reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
        },
    })
    .await
    .unwrap();
    repo.finish_execution(&scope, ids::logical("logical"), "attempt", true)
        .await
        .unwrap();
    repo.review_training_fixture(&ReviewRequest {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        intent: "create an item".into(),
        subtasks: vec![AchievedSubtask {
            intent: "create an item".into(),
            observed_outcome: "created item observed".into(),
            execution_ids: vec![ids::run("sensitive-run")],
        }],
    })
    .await
    .unwrap();
    let claim = repo.claim_scoped(60, Some(&scope)).await.unwrap().unwrap();
    let batch = candidates(&repo, &claim, &WithheldSourceModel)
        .await
        .unwrap();
    assert_eq!(batch.associations.len(), 1);
    assert_eq!(batch.examples.len(), 1);
    assert!(matches!(
        batch.examples[0].state,
        CandidateState::Proposed { .. }
    ));
    assert!(batch.examples[0].program().is_none());
    assert_eq!(
        repo.normalization_audits(&scope, 10).await.unwrap().len(),
        2
    );
    repo.finish_with_guidance(
        &claim,
        &batch.examples,
        &batch.associations,
        PublicationPolicy::Automatic,
    )
    .await
    .unwrap();
    assert!(repo.list_eligible(&scope, 3).await.unwrap().is_empty());
    assert_eq!(
        repo.frozen_guidance(&scope, claim.id)
            .await
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    drop(repo);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
