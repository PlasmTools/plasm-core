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
) -> Result<NormalizedProposal, LearningError> {
    let families: Vec<_> = material
        .existing
        .iter()
        .map(|e| e.candidate.family.clone())
        .chain(material.guidance.iter().map(|g| g.family.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(16)
        .collect();
    let candidates: Vec<_> = material
        .existing
        .iter()
        .filter(|e| families.contains(&e.candidate.family))
        .map(|e| {
            serde_json::json!({"family":e.candidate.family,"intent":e.candidate.intent,
            "catalogues":e.candidate.catalog_fingerprints.keys().collect::<Vec<_>>(),
            "teaching_example":e.candidate.program()})
        })
        .collect();
    let guidance: Vec<_> = material.guidance.iter().filter(|g| families.contains(&g.family))
        .map(|g| serde_json::json!({"family":g.family,"intent":g.intent(),"requirements":g.requirements()})).collect();
    let observed = material
        .observation
        .and_then(ProgramObservation::available)
        .map(|observed| {
            serde_json::json!({"source":observed.model_source(),
            "dependencies":observed.program().dependencies,
            "outputs":observed.program().outputs})
        });
    let request = model.normalization_protocol()?.request(
        serde_json::json!({"subtask":intent,"existing":candidates,"catalogue_associations":guidance,
            "observed_program":observed,"catalogue_requirements":material.requirements}),
        families.into_iter().collect(),
    )?;
    let pending = repo.begin_normalization(context, &request).await?;
    let NormalizationExchange { response, capture } = model.normalize(&request).await;
    match response {
        Ok(raw) => match request.decode(&raw) {
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
    let mut proposed = BTreeMap::new();
    let mut associations = Vec::new();
    let cited: BTreeSet<_> = claim
        .seal
        .request
        .subtasks
        .iter()
        .flat_map(|s| s.run_ids.iter())
        .collect();
    let groups = super::quality::program_observations(
        claim
            .seal
            .executions
            .iter()
            .filter(|r| cited.contains(&r.run_id)),
    )?;
    for executions in groups {
        let execution = executions[0];
        let Some(requirements) = execution.catalogue_requirements.available() else {
            continue;
        };
        let run_ids: Vec<_> = executions.iter().map(|r| r.run_id).collect();
        let applicable: Vec<_> = claim
            .seal
            .request
            .subtasks
            .iter()
            .enumerate()
            .filter(|(_, s)| s.run_ids.iter().any(|id| run_ids.contains(id)))
            .collect();
        let subtask_index = applicable[0].0;
        let intent = serde_json::to_string(&serde_json::json!({
            "reviewed_outcomes":applicable.iter().map(|(_, s)| *s).collect::<Vec<_>>()
        }))?;
        let anchor_run = applicable[0]
            .1
            .run_ids
            .iter()
            .find(|id| run_ids.contains(id))
            .expect("cited program");
        let embedding_profile = model.embedding_profile();
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
        )
        .await?;
        let embedding = model.embed(initial.intent()).await?;
        let allowed = crate::discovery_store::DiscoveryAuthorization {
            catalogs: requirements
                .catalog_fingerprints()
                .keys()
                .cloned()
                .collect(),
            capabilities: BTreeMap::new(),
        };
        let compared = repo
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
        let interpretation = interpret(
            repo,
            NormalizationContext::review(
                claim,
                subtask_index,
                NormalizationStage::Compare {
                    run_id: *anchor_run,
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
        )
        .await?;
        let embedding = model.embed(interpretation.intent()).await?;
        let (intent, family_match) = interpretation.into_parts();
        associations.push(CatalogueAssociation::from_review(
            &claim.seal,
            &run_ids,
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
        let quality = Quality::observed(&claim.seal, &run_ids, teaching_tokens)?;
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
            Ok(checked) if matches!(family_match, FamilyMatch::Uncertain) => {
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

async fn recall_with_model(
    repo: &WorkflowRepository,
    scope: &LibraryScope,
    intent: &str,
    allowed: &crate::discovery_store::DiscoveryAuthorization,
    pins: &BTreeMap<String, String>,
    model: &dyn ProposalModel,
) -> Result<(Vec<StoredExample>, Vec<CatalogueAssociation>), LearningError> {
    if !repo.has_active_learning(scope).await? {
        return Ok((Vec::new(), Vec::new()));
    }
    let normalized = interpret(
        repo,
        NormalizationContext::recall(scope.clone()),
        model,
        intent,
        InterpretationMaterial::default(),
    )
    .await?
    .into_parts()
    .0;
    let embedding = model.embed(&normalized).await?;
    let profile = model.embedding_profile();
    let guidance = repo
        .search_guidance(
            scope,
            super::repository::GuidanceSearch {
                intent: &normalized,
                embedding: &embedding,
                profile: &profile,
                allowed,
                catalog_fingerprints: pins,
                limit: 16,
            },
        )
        .await?;
    let examples: Vec<_> = repo
        .search(
            scope,
            super::repository::ExampleSearch {
                intent: &normalized,
                embedding: &embedding,
                profile: &profile,
                catalogs: &allowed.catalogs,
                limit: 16,
                active_only: true,
            },
        )
        .await?
        .into_iter()
        .filter(|e| {
            e.candidate.program().is_some_and(|p| {
                p.required_capabilities.iter().all(|r| allowed.permits(r))
                    && p.dependencies
                        .iter()
                        .all(|d| allowed.catalogs.contains(&d.entry_id))
                    && p.catalog_fingerprints
                        .iter()
                        .all(|(entry, hash)| pins.get(entry) == Some(hash))
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
    let mut coverage = BTreeSet::new();
    let examples = ranked
        .into_iter()
        .filter_map(|r| examples.iter().find(|e| e.id.to_string() == r.id).cloned())
        .filter(|e| coverage.insert(e.candidate.coverage.clone()))
        .take(3)
        .collect();
    Ok((examples, guidance))
}
impl WorkflowLearning {
    pub async fn recall_with_guidance(
        &self,
        scope: &LibraryScope,
        intent: &str,
        allowed: &crate::discovery_store::DiscoveryAuthorization,
        pins: &BTreeMap<String, String>,
    ) -> Result<(Vec<StoredExample>, Vec<CatalogueAssociation>), LearningError> {
        if self.mode != LearningMode::Serving {
            return Ok((Vec::new(), Vec::new()));
        }
        let repo = self.repository().await?;
        recall_with_model(&repo, scope, intent, allowed, pins, &ConfiguredProvider).await
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
