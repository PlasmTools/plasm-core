//! Bounded proposal worker. The only publication authority is the verified ledger.
use super::catalogue_guidance::CatalogueAssociation;
use super::normalization::{
    FamilyMatch, NormalizationChat, NormalizationError, NormalizationRequest, NormalizedProposal,
};
use super::repository::{
    NormalizationCapture, NormalizationContext, NormalizationOutcome, NormalizationStage,
};
use super::teaching::{CheckedTeachingExample, ProgramCatalogueRequirements, ProgramObservation};
use super::{
    index::{self, IndexEntry, NormalizedIntent},
    model::*,
    repository::WorkflowRepository,
    teaching::TeachingProgram,
    LearningError, LearningMode, WorkflowLearning,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// The operation which exhausted the bounded optional recall budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStage {
    Catalogue,
    Repository,
    Library,
    Normalization,
    Embedding,
    Search,
    Applicability,
}

impl std::fmt::Display for RecallStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

struct RecallBudget {
    deadline: tokio::time::Instant,
}

impl RecallBudget {
    fn new(duration: Duration) -> Self {
        Self {
            deadline: tokio::time::Instant::now() + duration,
        }
    }

    async fn run<T>(
        &self,
        stage: RecallStage,
        limit: Duration,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, LearningError> {
        let started = std::time::Instant::now();
        let deadline = self.deadline.min(tokio::time::Instant::now() + limit);
        let result = tokio::time::timeout_at(deadline, future).await;
        tracing::debug!(target: "plasm_agent::workflow_learning", ?stage, elapsed_ms = started.elapsed().as_millis(), timed_out = result.is_err(), "teaching recall stage completed");
        result.map_err(|_| LearningError::RecallTimeout(stage))
    }
}

struct NormalizationExchange {
    response: Result<String, LearningError>,
    capture: NormalizationCapture,
}

trait ProposalModel {
    fn embedding_profile(&self) -> String;
    fn normalization_protocol(&self) -> Result<NormalizationChat, NormalizationError> {
        NormalizationChat::configured(None)
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange>;
    fn embed<'a>(
        &'a self,
        intent: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>>;
    fn assess<'a>(
        &'a self,
        _request: &'a super::applicability::Request,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<BTreeMap<String, super::applicability::Assessment>, LearningError>,
    > {
        Box::pin(async {
            Err(LearningError::ProviderConfiguration(
                "teaching applicability is unavailable".into(),
            ))
        })
    }
}

struct ConfiguredProvider;
impl ProposalModel for ConfiguredProvider {
    fn embedding_profile(&self) -> String {
        embedding_profile()
    }
    fn normalization_protocol(&self) -> Result<NormalizationChat, NormalizationError> {
        NormalizationChat::configured(std::env::var("PLASM_WORKFLOW_MODEL").ok())
    }
    fn normalize<'a>(
        &'a self,
        request: &'a NormalizationRequest,
    ) -> futures_util::future::LocalBoxFuture<'a, NormalizationExchange> {
        Box::pin(async move {
            let mut capture = NormalizationCapture::default();
            let response = async {
                let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
                    LearningError::ProviderConfiguration(
                        "normalization provider is not configured".into(),
                    )
                })?;
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(90))
                    .build()?;
                let transport = plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                    client,
                    request.endpoint(),
                    key,
                );
                Ok(plasm_runtime::provider_transport::request_attempt(
                    &transport,
                    request.body(),
                    0,
                    std::time::Duration::from_secs(90),
                    |attempt| {
                        capture.record(attempt);
                        Ok(())
                    },
                )
                .await?)
            }
            .await;
            if let Err(error) = &response {
                capture.failure(error);
            }
            NormalizationExchange { response, capture }
        })
    }
    fn embed<'a>(
        &'a self,
        intent: &'a NormalizedIntent,
    ) -> futures_util::future::LocalBoxFuture<'a, Result<Vec<f32>, LearningError>> {
        Box::pin(embed(intent))
    }
    fn assess<'a>(
        &'a self,
        request: &'a super::applicability::Request,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<BTreeMap<String, super::applicability::Assessment>, LearningError>,
    > {
        Box::pin(async move {
            let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
                LearningError::ProviderConfiguration(
                    "applicability provider is not configured".into(),
                )
            })?;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .build()?;
            let transport = plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                client,
                crate::discovery_matcher::RELEVANCE_ENDPOINT,
                key,
            );
            let raw = plasm_runtime::provider_transport::request_provider(
                &transport,
                request.body(),
                plasm_runtime::provider_transport::ProviderRetryPolicy {
                    attempts: 2,
                    deadline: Duration::from_secs(9),
                    attempt_timeout: Duration::from_secs(8),
                    backoff: Duration::from_millis(200),
                },
                |_| Ok(()),
            )
            .await?;
            request.decode(&raw).map_err(LearningError::Applicability)
        })
    }
}

#[derive(Default)]
struct InterpretationMaterial<'a> {
    existing: &'a [StoredExample],
    guidance: &'a [CatalogueAssociation],
    observation: Option<&'a ProgramObservation>,
    requirements: Option<&'a ProgramCatalogueRequirements>,
}
async fn interpret(
    repo: &WorkflowRepository,
    context: NormalizationContext,
    model: &dyn ProposalModel,
    intent: &str,
    material: InterpretationMaterial<'_>,
    model_timeout: Option<Duration>,
) -> Result<NormalizedProposal, LearningError> {
    let mut seen_families = BTreeSet::new();
    let families: Vec<_> = material
        .existing
        .iter()
        .map(|e| e.candidate.family.clone())
        .chain(material.guidance.iter().map(|g| g.family.clone()))
        .filter(|family| seen_families.insert(family.clone()))
        .take(16)
        .collect();
    let family_labels: std::collections::BTreeMap<String, String> = families
        .iter()
        .enumerate()
        .map(|(i, family)| (format!("family_{}", i + 1), family.clone()))
        .collect();
    let label_for = |family: &str| {
        family_labels
            .iter()
            .find_map(|(label, value)| (value == family).then_some(label))
            .expect("family selected from alternatives")
    };
    let candidates: Vec<_> = material
        .existing
        .iter()
        .filter(|e| families.contains(&e.candidate.family))
        .map(|e| {
            let teaching = e.candidate.program().map(|program| {
                let symbols: BTreeSet<_> = program
                    .occurrences
                    .iter()
                    .map(|item| format!("{}: {}", item.expected, item.reference))
                    .collect();
                serde_json::json!({"source":program.source,"symbols":symbols})
            });
            serde_json::json!({"family":label_for(&e.candidate.family),"intent":e.candidate.intent,
            "catalogues":e.candidate.catalog_fingerprints.keys().collect::<Vec<_>>(),
            "teaching_program":teaching})
        })
        .collect();
    let guidance: Vec<_> = material.guidance.iter().filter(|g| families.contains(&g.family))
        .map(|g| serde_json::json!({"family":label_for(&g.family),"intent":g.intent(),"catalogue_definitions":g.requirements().capability_contracts().iter().map(|c| c.teaching_text()).collect::<Vec<_>>()})).collect();
    let observed = material
        .observation
        .and_then(ProgramObservation::available)
        .map(|observed| {
            let symbols: BTreeSet<_> = observed
                .program()
                .occurrences
                .iter()
                .map(|item| format!("{}: {}", item.expected, item.reference))
                .collect();
            serde_json::json!({"source":observed.model_source(), "symbols":symbols})
        });
    let request = model.normalization_protocol()?.request(
        serde_json::json!({"subtask":intent,"existing":candidates,"catalogue_associations":guidance,
            "observed_program":observed,"catalogue_definitions":material.requirements.map(|r| r.capability_contracts().iter().map(|c| c.teaching_text()).collect::<Vec<_>>())}),
        family_labels.keys().cloned().collect(),
    )?;
    let pending = repo.begin_normalization(context, &request).await?;
    let NormalizationExchange { response, capture } = if let Some(timeout) = model_timeout {
        match tokio::time::timeout(timeout, model.normalize(&request)).await {
            Ok(exchange) => exchange,
            Err(_) => {
                let error = LearningError::RecallTimeout(RecallStage::Normalization);
                let mut capture = NormalizationCapture::default();
                capture.failure(&error);
                NormalizationExchange {
                    response: Err(error),
                    capture,
                }
            }
        }
    } else {
        model.normalize(&request).await
    };
    match response {
        Ok(raw) => match request
            .decode(&raw)
            .and_then(|proposal| proposal.resolve_family_labels(&family_labels))
        {
            Ok(proposal) => {
                repo.finish_normalization(
                    &pending,
                    capture,
                    NormalizationOutcome::Accepted {
                        proposal: proposal.clone(),
                    },
                )
                .await?;
                Ok(proposal)
            }
            Err(error) => {
                repo.finish_normalization(
                    &pending,
                    capture,
                    NormalizationOutcome::Rejected {
                        diagnostic: error.diagnostic(),
                    },
                )
                .await?;
                Err(error.into())
            }
        },
        Err(error) => {
            repo.finish_normalization(&pending, capture, NormalizationOutcome::TransportFailed)
                .await?;
            Err(error)
        }
    }
}

async fn embed(intent: &NormalizedIntent) -> Result<Vec<f32>, LearningError> {
    let client =
        plasm_catalog::embeddings::EmbeddingClient::from_env().map_err(LearningError::from)?;
    let text = format!("{} {}", intent.purpose, intent.facets.join(" "));
    client
        .embed(&[text], |_| Ok(()))
        .await
        .map_err(LearningError::from)?
        .into_iter()
        .next()
        .ok_or_else(|| LearningError::ProviderResponse("empty embedding response".into()))
}

fn embedding_profile() -> String {
    // EmbeddingClient::from_env uses precisely this profile; include renderer version.
    let profile = plasm_core::catalog_discovery::EmbeddingProfile::default();
    format!(
        "workflow-intent-v1:{}:{}:{}",
        profile.model, profile.dimensions, profile.encoding_format
    )
}

fn transport_retryable(error: &plasm_runtime::provider_transport::ProviderTransportError) -> bool {
    use plasm_runtime::provider_transport::ProviderTransportError;
    match error {
        ProviderTransportError::RetryExhausted { source, .. } => transport_retryable(source),
        ProviderTransportError::DeadlineExpired => true,
        other => other.retry_delay(0).is_some(),
    }
}

fn retryable(error: &LearningError) -> bool {
    match error {
        LearningError::Normalization(NormalizationError::Truncated) => true,
        LearningError::Provider(error) => transport_retryable(error),
        LearningError::Embedding(
            plasm_catalog::embeddings::EmbeddingAcquisitionError::Transport(error),
        ) => transport_retryable(error),
        LearningError::Repository(super::repository::RepositoryError::PortfolioConflict) => true,
        LearningError::Repository(super::repository::RepositoryError::Database(error)) => {
            match error {
                sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
                sqlx::Error::Database(error) => error.code().is_some_and(|code| {
                    code.starts_with("08")
                        || matches!(
                            code.as_ref(),
                            "40001" | "40P01" | "57P01" | "57P02" | "57P03"
                        )
                }),
                _ => false,
            }
        }
        _ => false,
    }
}

fn coverage(program: &TeachingProgram) -> Result<Vec<String>, LearningError> {
    let mut contracts = BTreeSet::new();
    for capability in &program.required_capabilities {
        contracts.insert(format!("capability:{}", serde_json::to_string(capability)?));
    }
    for output in &program.outputs {
        // Node IDs are compilation artifacts, not semantic coverage.
        contracts.insert(format!(
            "output:{}",
            serde_json::to_string(&(&output.shape, &output.row_type))?
        ));
    }
    Ok(contracts.into_iter().collect())
}

type ProposalKey = (String, String, Vec<String>, String);
fn ambiguous_new_family(
    compared: &[StoredExample],
    intent: &NormalizedIntent,
    fingerprints: &BTreeMap<String, String>,
    identity: &str,
    coverage: &[String],
) -> bool {
    compared.iter().any(|existing| {
        existing.candidate.program().is_some()
            && &existing.candidate.catalog_fingerprints == fingerprints
            && existing.candidate.identity != identity
            && existing.candidate.coverage == coverage
            && index::normalize(&existing.candidate.intent.purpose)
                == index::normalize(&intent.purpose)
    })
}
fn select_candidate(proposed: &mut BTreeMap<ProposalKey, Candidate>, candidate: Candidate) {
    let key = (
        candidate.family.clone(),
        candidate.variant.clone(),
        candidate.coverage.clone(),
        candidate
            .behavior_fingerprint()
            .unwrap_or_default()
            .to_owned(),
    );
    match proposed.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(candidate);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let previous = entry.get();
            let candidate_eligible = candidate.eligible(candidate.quality.assurance);
            let previous_eligible = previous.eligible(previous.quality.assurance);
            if candidate_eligible && !previous_eligible
                || candidate_eligible == previous_eligible
                    && (candidate.quality.strictly_better_than(&previous.quality)
                        || candidate.quality == previous.quality
                            && candidate.identity < previous.identity)
            {
                entry.insert(candidate);
            }
        }
    }
}

fn entries(examples: &[StoredExample]) -> Vec<IndexEntry> {
    examples
        .iter()
        .map(|e| IndexEntry {
            id: e.id.to_string(),
            intent: e.candidate.intent.clone(),
            catalogs: e.candidate.catalog_fingerprints.keys().cloned().collect(),
            embedding: Some(e.candidate.embedding.clone()),
        })
        .collect()
}

struct CurationBatch {
    examples: Vec<Candidate>,
    associations: Vec<CatalogueAssociation>,
}
async fn candidates(
    repo: &WorkflowRepository,
    claim: &ClaimedReview,
    model: &dyn ProposalModel,
) -> Result<CurationBatch, LearningError> {
    let scope = &claim.seal.request.scope;
    let mut proposed: BTreeMap<ProposalKey, Candidate> = BTreeMap::new();
    let mut associations = Vec::new();
    let cited: BTreeSet<_> = claim
        .seal
        .request
        .subtasks
        .iter()
        .flat_map(|s| s.execution_ids.iter())
        .collect();
    let groups = super::quality::program_observations(
        claim
            .seal
            .executions
            .iter()
            .filter(|r| cited.contains(&r.execution_id)),
    )?;
    for executions in groups {
        let execution = executions[0];
        let Some(requirements) = execution.catalogue_requirements.available() else {
            continue;
        };
        let execution_ids: Vec<_> = executions.iter().map(|r| r.execution_id).collect();
        let applicable: Vec<_> = claim
            .seal
            .request
            .subtasks
            .iter()
            .enumerate()
            .filter(|(_, s)| s.execution_ids.iter().any(|id| execution_ids.contains(id)))
            .collect();
        let subtask_index = applicable[0].0;
        let intent = serde_json::to_string(&serde_json::json!({
            "proposed_purposes":applicable.iter().map(|(_, s)| &s.intent).collect::<Vec<_>>()
        }))?;
        let anchor_run = applicable[0]
            .1
            .execution_ids
            .iter()
            .find(|id| execution_ids.contains(id))
            .expect("cited program");
        let embedding_profile = model.embedding_profile();
        repo.ensure_review_training(claim).await?;
        let initial = interpret(
            repo,
            NormalizationContext::review(claim, subtask_index, NormalizationStage::Initial)?,
            model,
            &intent,
            InterpretationMaterial {
                observation: Some(&execution.observation),
                requirements: Some(requirements),
                ..Default::default()
            },
            None,
        )
        .await?;
        repo.ensure_review_training(claim).await?;
        let embedding = model.embed(initial.intent()).await?;
        let allowed = crate::discovery_store::DiscoveryAuthorization {
            catalogs: requirements
                .catalog_fingerprints()
                .keys()
                .cloned()
                .collect(),
            capabilities: BTreeMap::new(),
        };
        let mut compared = repo
            .search(
                scope,
                super::repository::ExampleSearch {
                    intent: initial.intent(),
                    embedding: &embedding,
                    profile: &embedding_profile,
                    catalogs: &allowed.catalogs,
                    limit: 16,
                    active_only: false,
                },
            )
            .await?;
        // Reviewed programs in this batch are not in the repository yet. Make
        // compatible verified siblings visible to the same semantic comparison
        // used for previously published material.
        let mut siblings = Vec::new();
        for sibling in proposed.values() {
            if matches!(sibling.state, CandidateState::Verified { .. })
                && &sibling.catalog_fingerprints == requirements.catalog_fingerprints()
                && !compared
                    .iter()
                    .any(|item| item.candidate.family == sibling.family)
            {
                siblings.push(StoredExample {
                    id: uuid::Uuid::nil(),
                    candidate: sibling.clone(),
                });
            }
        }
        siblings.append(&mut compared);
        let compared = siblings;
        let guidance = repo
            .search_guidance(
                scope,
                super::repository::GuidanceSearch {
                    intent: initial.intent(),
                    embedding: &embedding,
                    profile: &embedding_profile,
                    allowed: &allowed,
                    catalog_fingerprints: requirements.catalog_fingerprints(),
                    limit: 16,
                },
            )
            .await?;
        repo.ensure_review_training(claim).await?;
        let interpretation = interpret(
            repo,
            NormalizationContext::review(
                claim,
                subtask_index,
                NormalizationStage::Compare {
                    execution_id: *anchor_run,
                },
            )?,
            model,
            &intent,
            InterpretationMaterial {
                existing: &compared,
                guidance: &guidance,
                observation: Some(&execution.observation),
                requirements: Some(requirements),
            },
            None,
        )
        .await?;
        repo.ensure_review_training(claim).await?;
        let embedding = model.embed(interpretation.intent()).await?;
        let (intent, family_match) = interpretation.into_parts();
        associations.push(CatalogueAssociation::from_review(
            &claim.seal,
            &execution_ids,
            intent.clone(),
            family_match.clone(),
            embedding.clone(),
            embedding_profile.clone(),
        )?);
        // Teaching is a separate decision, after semantic curation and catalogue evidence.
        let family = match &family_match {
            FamilyMatch::Existing { family } => family.as_str().to_owned(),
            FamilyMatch::New | FamilyMatch::Uncertain => intent.family_key(),
        };
        let checked = CheckedTeachingExample::from_observation(&execution.observation);
        let fingerprints = checked
            .as_ref()
            .map(|c| c.program().catalog_fingerprints.clone())
            .unwrap_or_else(|_| requirements.catalog_fingerprints().clone());
        let variant =
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&fingerprints)?);
        let expected_family_version = repo.portfolio_version(scope, &family, &variant).await?;
        let (identity, coverage, teaching_tokens) = match &checked {
            Ok(c) => (
                c.program().structural_fingerprint.clone(),
                coverage(c.program())?,
                u32::try_from(plasm_core::o200k_token_count(&c.program().source))
                    .unwrap_or(u32::MAX),
            ),
            Err(_) => (requirements.source_sha256().to_owned(), Vec::new(), 0),
        };
        let quality = Quality::observed(&claim.seal, &execution_ids, teaching_tokens)?;
        // Identical stated purpose and executable coverage are not proof that
        // different programs have equivalent behavior. If semantic comparison
        // nevertheless proposes a new family, require review rather than
        // publishing an apparently redundant portfolio automatically.
        let ambiguous_new_family = matches!(family_match, FamilyMatch::New)
            && ambiguous_new_family(&compared, &intent, &fingerprints, &identity, &coverage);
        let state = match checked {
            Err(error) => CandidateState::Proposed {
                diagnostic: ProposalDiagnostic {
                    kind: match &error {
                        super::teaching::TeachingError::SensitiveOperand { .. } => {
                            DiagnosticKind::SensitiveOperand
                        }
                        super::teaching::TeachingError::UnresolvedSensitiveDataflow { .. } => {
                            DiagnosticKind::DisclosureUncertainty
                        }
                        _ => DiagnosticKind::UnsupportedProgram,
                    },
                    message: error.to_string(),
                },
            },
            Ok(checked)
                if matches!(family_match, FamilyMatch::Uncertain) || ambiguous_new_family =>
            {
                CandidateState::Uncertain {
                    program: checked,
                    diagnostic: ProposalDiagnostic {
                        kind: DiagnosticKind::SemanticUncertainty,
                        message: "semantic family is uncertain".into(),
                    },
                }
            }
            Ok(checked) => CandidateState::Verified {
                evidence: VerifiedProgram::observed(checked, claim.seal.assurance),
            },
        };
        select_candidate(
            &mut proposed,
            Candidate {
                family,
                variant,
                identity,
                intent,
                embedding,
                embedding_profile,
                coverage,
                quality,
                state,
                catalog_fingerprints: fingerprints,
                expected_family_version,
            },
        );
    }
    Ok(CurationBatch {
        examples: proposed.into_values().collect(),
        associations,
    })
}
async fn proposals_for_claim(
    repo: &WorkflowRepository,
    claim: &ClaimedReview,
    model: &dyn ProposalModel,
) -> Result<CurationBatch, LearningError> {
    if let Some(examples) = repo
        .frozen_candidates(&claim.seal.request.scope, claim.id)
        .await?
    {
        let associations = repo
            .frozen_guidance(&claim.seal.request.scope, claim.id)
            .await?
            .unwrap_or_default();
        return Ok(CurationBatch {
            examples,
            associations,
        });
    }
    candidates(repo, claim, model).await
}

pub(super) async fn tick(owner: &WorkflowLearning) -> Result<(), LearningError> {
    tick_scoped(owner, None).await
}

pub(super) async fn tick_scoped(
    owner: &WorkflowLearning,
    scope: Option<&LibraryScope>,
) -> Result<(), LearningError> {
    let provider_available = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty());
    let repo = owner.repository().await?;
    let claim = if provider_available {
        repo.claim_scoped(300, scope).await?
    } else {
        repo.claim_recorded_scoped(300, scope).await?
    };
    let Some(claim) = claim else { return Ok(()) };
    repo.ensure_review_training(&claim).await?;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        proposals_for_claim(&repo, &claim, &ConfiguredProvider),
    )
    .await;
    match result {
        Ok(Ok(batch)) => {
            if let Err(error) = repo
                .finish_with_guidance(
                    &claim,
                    &batch.examples,
                    &batch.associations,
                    match owner.mode {
                        LearningMode::Reviewed => PublicationPolicy::Reviewed,
                        LearningMode::Serving => PublicationPolicy::Automatic,
                        _ => PublicationPolicy::Shadow,
                    },
                )
                .await
            {
                let error = LearningError::Repository(error);
                repo.job_failure(&claim, &error.to_string(), retryable(&error))
                    .await?;
                return Err(error);
            }
        }
        Ok(Err(error)) => {
            repo.job_failure(&claim, &error.to_string(), retryable(&error))
                .await?;
            return Err(error);
        }
        Err(_) => {
            repo.job_failure(&claim, "promotion time budget exhausted", true)
                .await?
        }
    }
    Ok(())
}

fn effect_contracts(
    program: &TeachingProgram,
    catalogs: &BTreeMap<String, plasm_core::CGS>,
) -> Option<Vec<super::applicability::EffectContract>> {
    use plasm_core::schema::{CapabilityKind, OutputType};
    let mut effects = Vec::new();
    for reference in &program.required_capabilities {
        let capability = catalogs
            .get(&reference.catalog)?
            .capabilities
            .get(reference.capability.as_str())?;
        let outcome =
            capability
                .output_schema
                .as_ref()
                .and_then(|output| match &output.output_type {
                    OutputType::SideEffect { description } if !description.trim().is_empty() => {
                        Some(description.clone())
                    }
                    _ => None,
                });
        if matches!(
            capability.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        ) || outcome.is_some()
        {
            if capability.description.trim().is_empty() && outcome.is_none() {
                return None;
            }
            effects.push(super::applicability::EffectContract {
                capability: format!("{}/{}", reference.catalog, reference.capability),
                kind: capability.kind,
                description: capability.description.clone(),
                outcome,
            });
        }
    }
    Some(effects)
}

struct RecallCatalogue<'a> {
    pins: &'a BTreeMap<String, String>,
    graphs: &'a BTreeMap<String, plasm_core::CGS>,
}

async fn recall_with_model(
    repo: &WorkflowRepository,
    scope: &LibraryScope,
    intent: &str,
    allowed: &crate::discovery_store::DiscoveryAuthorization,
    catalogue: &RecallCatalogue<'_>,
    model: &dyn ProposalModel,
    budget: &RecallBudget,
) -> Result<(Vec<StoredExample>, Vec<CatalogueAssociation>), LearningError> {
    if !budget
        .run(
            RecallStage::Library,
            Duration::from_secs(5),
            repo.has_active_learning(scope),
        )
        .await??
    {
        return Ok((Vec::new(), Vec::new()));
    }
    let normalized = budget
        .run(
            RecallStage::Normalization,
            Duration::from_secs(12),
            interpret(
                repo,
                NormalizationContext::recall(scope.clone()),
                model,
                intent,
                InterpretationMaterial::default(),
                Some(Duration::from_secs(10)),
            ),
        )
        .await??
        .into_parts()
        .0;
    let embedding = budget
        .run(
            RecallStage::Embedding,
            Duration::from_secs(8),
            model.embed(&normalized),
        )
        .await??;
    let profile = model.embedding_profile();
    let search_guidance = repo.search_guidance(
        scope,
        super::repository::GuidanceSearch {
            intent: &normalized,
            embedding: &embedding,
            profile: &profile,
            allowed,
            catalog_fingerprints: catalogue.pins,
            limit: 16,
        },
    );
    let search_examples = repo.search(
        scope,
        super::repository::ExampleSearch {
            intent: &normalized,
            embedding: &embedding,
            profile: &profile,
            catalogs: &allowed.catalogs,
            limit: 16,
            active_only: true,
        },
    );
    let (guidance, found_examples) = budget
        .run(RecallStage::Search, Duration::from_secs(5), async {
            tokio::try_join!(search_guidance, search_examples)
        })
        .await??;
    let examples: Vec<_> = found_examples
        .into_iter()
        .filter(|e| {
            e.candidate.program().is_some_and(|p| {
                p.required_capabilities.iter().all(|r| allowed.permits(r))
                    && p.dependencies
                        .iter()
                        .all(|d| allowed.catalogs.contains(&d.entry_id))
                    && p.catalog_fingerprints
                        .iter()
                        .all(|(entry, hash)| catalogue.pins.get(entry) == Some(hash))
            })
        })
        .collect();
    let ranked = index::rank(
        &normalized,
        Some(&embedding),
        &allowed.catalogs,
        None,
        &entries(&examples),
        16,
    );
    let examples: Vec<_> = ranked
        .into_iter()
        .filter_map(|r| examples.iter().find(|e| e.id.to_string() == r.id).cloned())
        .take(6)
        .collect();
    if examples.is_empty() && guidance.is_empty() {
        return Ok((examples, guidance));
    }
    // Associations nominate authorized catalogue candidates; the discovery
    // matcher judges each capability against the actual request. A whole-program
    // comparison here would discard useful operations from different workflows.
    for association in &guidance {
        tracing::debug!(target: "plasm_agent::workflow_learning", family = %association.family, variant = %association.variant, decision = "nominate_for_discovery", "catalogue guidance candidate decision");
    }
    if examples.is_empty() {
        return Ok((examples, guidance));
    }
    use super::applicability::{Candidate as ApplicabilityCandidate, CandidateKind, Request};
    let mut candidates = Vec::new();
    for (index, example) in examples.iter().enumerate() {
        let Some(program) = example.candidate.program() else {
            continue;
        };
        let Some(effects) = effect_contracts(program, catalogue.graphs) else {
            tracing::debug!(target: "plasm_agent::workflow_learning", example = %example.id, decision = "unresolved_effect_contract", "teaching example candidate decision");
            continue;
        };
        candidates.push(ApplicabilityCandidate {
            label: format!("example_{}", index + 1),
            kind: CandidateKind::Example,
            purpose: example.candidate.intent.purpose.clone(),
            dependencies: program
                .required_capabilities
                .iter()
                .map(|cap| format!("{}/{}", cap.catalog, cap.capability))
                .collect(),
            symbols: program
                .occurrences
                .iter()
                .map(|item| format!("{}: {}", item.expected, item.reference))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            outputs: program.outputs.iter().map(ToString::to_string).collect(),
            effects,
            source: Some(program.source.clone()),
        });
    }
    let request = Request::bounded(
        crate::discovery_matcher::RELEVANCE_MODEL,
        intent,
        &candidates,
    )
    .map_err(LearningError::Applicability)?;
    let Some(request) = request else {
        tracing::warn!(target: "plasm_agent::workflow_learning", candidates = candidates.len(), "no whole teaching candidate fits the semantic budget");
        for example in &examples {
            tracing::debug!(target: "plasm_agent::workflow_learning", example = %example.id, decision = "not_assessed_budget", "teaching example candidate decision");
        }
        return Ok((Vec::new(), guidance));
    };
    let judgments = match budget
        .run(
            RecallStage::Applicability,
            Duration::from_secs(10),
            model.assess(&request),
        )
        .await
    {
        Ok(Ok(judgments)) => judgments,
        _ => {
            for example in &examples {
                tracing::debug!(target: "plasm_agent::workflow_learning", example = %example.id, decision = "assessment_unavailable", "teaching example candidate decision");
            }
            tracing::debug!(target: "plasm_agent::workflow_learning", "example applicability unavailable; catalogue candidates retained");
            return Ok((Vec::new(), guidance));
        }
    };
    let useful = judgments
        .values()
        .filter(|assessment| assessment.supports_teaching())
        .count();
    let uncertain = judgments
        .values()
        .filter(|assessment| {
            matches!(
                assessment.judgment,
                super::applicability::Judgment::Uncertain
            ) || matches!(
                assessment.effect_alignment,
                super::applicability::EffectAlignment::Uncertain
            )
        })
        .count();
    tracing::debug!(target: "plasm_agent::workflow_learning", candidates = judgments.len(), useful, uncertain, "teaching applicability assessed");
    for (index, example) in examples.iter().enumerate() {
        let label = format!("example_{}", index + 1);
        let decision = judgments
            .get(&label)
            .map(|result| format!("{:?}/{:?}", result.judgment, result.effect_alignment))
            .unwrap_or_else(|| "not_assessed_budget".into());
        tracing::debug!(target: "plasm_agent::workflow_learning", example = %example.id, decision, "teaching example candidate decision");
    }
    let mut coverage = BTreeSet::new();
    let selected = super::applicability::supported_in_priority_order(examples.len(), &judgments)
        .into_iter()
        .filter_map(|index| examples.get(index))
        .filter(|example| coverage.insert(example.candidate.coverage.clone()))
        .take(3)
        .cloned()
        .collect();
    Ok((selected, guidance))
}
impl WorkflowLearning {
    pub async fn recall_with_guidance(
        &self,
        scope: &LibraryScope,
        intent: &str,
        allowed: &crate::discovery_store::DiscoveryAuthorization,
        store: &crate::discovery_store::DiscoveryStore,
        generation: &str,
    ) -> Result<(Vec<StoredExample>, Vec<CatalogueAssociation>), LearningError> {
        if !matches!(
            self.mode,
            LearningMode::Serving | LearningMode::ShadowWithRecall
        ) {
            return Ok((Vec::new(), Vec::new()));
        }
        let budget = RecallBudget::new(Duration::from_secs(30));
        let catalogs = budget
            .run(
                RecallStage::Catalogue,
                Duration::from_secs(5),
                store.load_discovery_generation(generation, allowed),
            )
            .await?
            .map_err(|error| {
                tracing::warn!(%error, "catalogue guidance pins unavailable");
                LearningError::Evidence("catalogue guidance pins unavailable".into())
            })?
            .0;
        let pins = catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), cgs.effective_catalog_cgs_hash_hex()))
            .collect();
        let repo = budget
            .run(
                RecallStage::Repository,
                Duration::from_secs(5),
                self.repository(),
            )
            .await??;
        recall_with_model(
            &repo,
            scope,
            intent,
            allowed,
            &RecallCatalogue {
                pins: &pins,
                graphs: &catalogs,
            },
            &ConfiguredProvider,
            &budget,
        )
        .await
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
