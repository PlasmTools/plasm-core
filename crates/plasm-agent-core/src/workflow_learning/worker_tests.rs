//! Real compiler, PostgreSQL ledger and session publication with a scripted model.
use super::*;
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
        let captured = receipt
            .symbol_context
            .captured()
            .expect("compiler-owned saved ledger");
        let ledger = plasm_core::PersistedSymbolLedger::decode(captured.ledger_bytes()).unwrap();
        assert_eq!(ledger.entities.first().map(String::as_str), Some("Product"));
        assert_eq!(
            ledger.entity_catalog_entry_ids.first().map(String::as_str),
            Some("matrix")
        );
        assert_eq!(
            ledger.catalog_cgs_hashes.iter().collect::<BTreeMap<_, _>>(),
            captured
                .catalog_fingerprints()
                .iter()
                .collect::<BTreeMap<_, _>>()
        );
        let emitted = collected["symbol_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["run_id"] == receipt.run_id.to_wire())
            .unwrap();
        assert_eq!(
            emitted["ledger_sha256"],
            plasm_core::catalog_discovery::content_hash(captured.ledger_bytes()),
            "durable seal preserves exact producer ledger bytes across process exit"
        );
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
    repo.publish_frozen(&scope, claim.id, &hashes, "offline-native-proof")
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
    repo.bind_session(&scope, ids::logical("logical"))
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
        run_id: ids::run("fixture-run"),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("fixture-execute"),
        run_artifact: serde_json::json!({"fixture_completed":true}),
        source: source.clone(),
        symbol_context: Default::default(),
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
    repo.record_execution(&receipt).await.unwrap();
    let mut other_output = receipt.clone();
    other_output.run_id = ids::run("fixture-other-output");
    other_output.run_artifact = serde_json::json!({"fixture_completed":true,"output":2});
    repo.record_execution(&other_output).await.unwrap();
    let request = ReviewRequest {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        intent: "Inspect one item".into(),
        subtasks: vec![
            AchievedSubtask {
                intent: "Inspect one item".into(),
                observed_outcome: "One complete item was returned".into(),
                run_ids: vec![ids::run("fixture-run")],
            },
            AchievedSubtask {
                intent: "Inspect another output".into(),
                observed_outcome: "The second result completed".into(),
                run_ids: vec![ids::run("fixture-other-output")],
            },
        ],
    };
    assert!(repo.review(&request).await.is_err());
    repo.finish_execution(&scope, ids::logical("logical"), "attempt", true)
        .await
        .unwrap();
    let review = repo.review(&request).await.unwrap();
    assert!(
        repo.claim_recorded_scoped(60, Some(&scope))
            .await
            .unwrap()
            .is_none(),
        "missing provider must leave fresh proposals unclaimed"
    );
    let claim = repo.claim(60).await.unwrap().unwrap();
    assert_eq!(claim.id, review.id);
    assert!(NormalizationContext::review(&claim, usize::MAX, NormalizationStage::Initial).is_err());
    assert!(NormalizationContext::review(
        &claim,
        0,
        NormalizationStage::Compare {
            run_id: ids::run("not-in-seal")
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
        execution.catalogue_requirements = Default::default();
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
    repo.approve_publication(&scope, review.id, "fixture-operator")
        .await
        .unwrap();
    let replay_claim = repo
        .claim_recorded_scoped(60, Some(&scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay_claim.id, review.id);
    let replay = proposals_for_claim(&repo, &replay_claim, &ForbiddenModel)
        .await
        .unwrap();
    repo.finish_with_guidance(
        &replay_claim,
        &replay.examples,
        &replay.associations,
        PublicationPolicy::Reviewed,
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(&original_artifacts).unwrap(),
        serde_json::to_value(repo.export_proposals(&scope).await.unwrap()).unwrap()
    );
    let (examples, guidance) = recall_with_model(
        &repo,
        &scope,
        "Inspect one item",
        &crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from([
            "langmatrix".into()
        ])),
        &receipt.catalog_fingerprints,
        &OfflineModel,
    )
    .await
    .unwrap();
    assert_eq!(examples.len(), 1);
    assert!(!guidance.is_empty());
    // A different complete authored program in the same logical session is a
    // separate candidate, even when review intent uses the same vocabulary.
    let second_source = super::super::teaching::tests::source(&session);
    let second_bundle =
        crate::plasm_dag::compile_python_program_checked(&session, &second_source).unwrap();
    let mut second_program = receipt.clone();
    second_program.run_id = ids::run("second-program-run");
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
    repo.record_execution(&second_program).await.unwrap();
    let mut next_request = request.clone();
    next_request.session_revision += 1;
    next_request.subtasks.push(AchievedSubtask {
        intent: "Inspect a single result in another complete program".into(),
        observed_outcome: "A complete single result was returned".into(),
        run_ids: vec![second_program.run_id],
    });
    repo.review(&next_request).await.unwrap();
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
fn provider_failures_retry_only_transient_transport() {
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
        assert!(input["catalogue_requirements"].is_object());
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
    repo.bind_session(&scope, ids::logical("logical"))
        .await
        .unwrap();
    repo.begin_execution(&scope, ids::logical("logical"), "attempt")
        .await
        .unwrap();
    repo.record_execution(&ExecutionReceipt {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        run_id: ids::run("sensitive-run"),
        plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
        prompt_hash: session.prompt_hash.parse().unwrap(),
        execute_session: ids::execute("execute"),
        run_artifact: serde_json::json!({"fixture_completed":true}),
        source,
        compiled_plan: serde_json::to_value(&bundle.artifact().comp).unwrap(),
        observation,
        catalog_fingerprints: requirements.catalog_fingerprints().clone(),
        catalogue_requirements:
            super::super::catalogue_guidance::CatalogueRequirementEvidence::Available(requirements),
        symbol_context: Default::default(),
    })
    .await
    .unwrap();
    repo.finish_execution(&scope, ids::logical("logical"), "attempt", true)
        .await
        .unwrap();
    repo.review(&ReviewRequest {
        scope: scope.clone(),
        logical_session: ids::logical("logical"),
        session_revision: 1,
        intent: "create an item".into(),
        subtasks: vec![AchievedSubtask {
            intent: "create an item".into(),
            observed_outcome: "created item observed".into(),
            run_ids: vec![ids::run("sensitive-run")],
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
