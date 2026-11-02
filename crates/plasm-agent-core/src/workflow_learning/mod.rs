//! Verified workflow learning. Provider proposals never bypass deterministic gates.
pub mod archive;
pub mod catalogue_guidance;
pub(crate) mod execution;
mod identity;
pub mod index;
pub mod model;
pub mod native;
pub mod native_admin;
pub mod native_recall;
pub mod normalization;
mod quality;
mod review;
#[cfg(test)]
pub(crate) mod test_identities;
pub(crate) use review::ReviewSession;
pub mod repository;
pub mod serving;
pub mod teaching;
mod worker;

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::server_state::PlasmHostState;
use model::{CompiledProgramId, ExecutionReceipt, LibraryScope, LogicalSessionId};
use repository::WorkflowRepository;

/// Evidence supplied by the host only after a successful completed execution.
pub struct CompletedExecution<'a> {
    pub session: &'a ExecuteSession,
    pub logical_session: LogicalSessionId,
    pub execute_session: &'a str,
    pub source: &'a str,
    pub bundle: &'a PlasmCompBundle,
    pub output: &'a PlasmPlanRunResult,
}
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearningMode {
    Disabled,
    Shadow,
    Reviewed,
    Serving,
}

#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    #[error(transparent)]
    PromptIdentity(#[from] crate::execute_path_ids::PromptHashParseError),
    #[error(transparent)]
    ExecuteIdentity(#[from] crate::execute_path_ids::ExecuteSessionIdParseError),
    #[error(transparent)]
    Normalization(#[from] normalization::NormalizationError),
    #[error("workflow learning is disabled")]
    Disabled,
    #[error(transparent)]
    Repository(#[from] repository::RepositoryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Quality(#[from] model::QualityError),
    #[error(transparent)]
    Provider(#[from] plasm_runtime::provider_transport::ProviderTransportError),
    #[error(transparent)]
    Embedding(#[from] plasm_catalog::embeddings::EmbeddingAcquisitionError),
    #[error(transparent)]
    ProviderClient(#[from] reqwest::Error),
    #[error("workflow provider configuration: {0}")]
    ProviderConfiguration(String),
    #[error("workflow provider response: {0}")]
    ProviderResponse(String),
    #[error("workflow evidence: {0}")]
    Evidence(String),
    #[error("workflow artifact retrieval failed: {0}")]
    ArtifactRetrieval(#[source] Box<crate::run_artifacts::RunArtifactError>),
    #[error("workflow artifact decoding failed: {0}")]
    ArtifactDecoding(#[source] Box<crate::run_artifacts::RunArtifactError>),
    #[error("workflow collection restoration failed: {0}")]
    CollectionRestoration(#[source] Box<plasm_core::collection_codec::CollectionFault>),
}

/// Lazy initialization keeps optional learning failures out of engine startup.
pub struct WorkflowLearning {
    pub mode: LearningMode,
    database_url: Option<String>,
    repository: OnceCell<Arc<WorkflowRepository>>,
    worker: std::sync::atomic::AtomicBool,
}

impl Default for WorkflowLearning {
    fn default() -> Self {
        let database_url = std::env::var("PLASM_WORKFLOW_DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let mode = match std::env::var("PLASM_WORKFLOW_LEARNING").as_deref() {
            Ok("shadow") if database_url.is_some() => LearningMode::Shadow,
            Ok("reviewed") if database_url.is_some() => LearningMode::Reviewed,
            Ok("serving") if database_url.is_some() => LearningMode::Serving,
            _ => LearningMode::Disabled,
        };
        Self {
            mode,
            database_url,
            repository: OnceCell::new(),
            worker: false.into(),
        }
    }
}

impl WorkflowLearning {
    /// Explicit host policy. Worker ownership is separate from native task lifetime.
    pub fn configured(mode: LearningMode, database_url: Option<String>) -> Self {
        Self {
            mode,
            database_url,
            repository: OnceCell::new(),
            worker: false.into(),
        }
    }

    #[cfg(test)]
    fn with_test_repository(mode: LearningMode, repository: Arc<WorkflowRepository>) -> Self {
        Self {
            mode,
            database_url: None,
            repository: OnceCell::new_with(Some(repository)),
            worker: false.into(),
        }
    }
    pub async fn begin_execution(
        &self,
        tenant: &str,
        logical: LogicalSessionId,
        id: &str,
    ) -> Result<Option<LibraryScope>, LearningError> {
        if !self.enabled() {
            return Ok(None);
        }
        let repository = self.repository().await?;
        let Some(scope) = repository
            .session_scope(&learning_tenant(tenant), logical)
            .await?
        else {
            return Ok(None);
        };
        repository.begin_execution(&scope, logical, id).await?;
        Ok(Some(scope))
    }

    pub async fn finish_execution(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
        id: &str,
        success: bool,
    ) -> Result<(), LearningError> {
        self.repository()
            .await?
            .finish_execution(scope, logical, id, success)
            .await?;
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.mode != LearningMode::Disabled
    }

    pub async fn repository(&self) -> Result<Arc<WorkflowRepository>, LearningError> {
        if !self.enabled() {
            return Err(LearningError::Disabled);
        }
        if let Some(repository) = self.repository.get() {
            return Ok(Arc::clone(repository));
        }
        let url = self
            .database_url
            .as_deref()
            .ok_or(LearningError::Disabled)?;
        self.repository
            .get_or_try_init(|| async {
                let repository = WorkflowRepository::connect(url).await?;
                repository.migrate().await?;
                Ok::<_, LearningError>(Arc::new(repository))
            })
            .await
            .cloned()
    }

    pub fn start_worker(self: &Arc<Self>) {
        if !self.enabled() || self.worker.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let learning = Arc::downgrade(self);
        // Background provider latency must not consume foreground execution permits.
        let pool = Arc::new(crate::live_plan_run_worker::LivePlanRunPool::new());
        tokio::spawn(async move {
            loop {
                let Some(owner) = learning.upgrade() else {
                    break;
                };
                let result = pool
                    .run_local(move || async move { worker::tick(&owner).await })
                    .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(target:"plasm_agent::workflow_learning",%error,"promotion attempt failed")
                    }
                    Err(error) => {
                        tracing::warn!(target:"plasm_agent::workflow_learning",%error,"promotion worker unavailable")
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    /// Record only after the orchestrator has observed a complete successful run.
    /// Failure to retain evidence never changes the execution's result.
    pub async fn record_completed(
        &self,
        host: &PlasmHostState,
        completed: CompletedExecution<'_>,
    ) -> Result<(), LearningError> {
        let CompletedExecution {
            session,
            logical_session,
            execute_session,
            source,
            bundle,
            output,
        } = completed;
        if !self.enabled() {
            return Ok(());
        }
        let repository = self.repository().await?;
        let tenant = learning_tenant(&session.tenant_scope);
        let Some(scope) = repository.session_scope(&tenant, logical_session).await? else {
            return Ok(());
        };
        // The learning unit is the complete authored program. Returned artifacts
        // are observations of that execution, never instructions to slice it.
        let observation = teaching::ProgramObservation::from_execution(session, source, bundle);
        let catalogue_requirements =
            catalogue_guidance::CatalogueRequirementEvidence::from_compiled(session, bundle);
        let symbol_context = crate::program_symbol_context::SavedProgramContext::from_compilation(
            bundle,
            &scope,
            &logical_session,
            execute_session,
        );
        let mut catalogs = BTreeMap::new();
        for (entry, context) in &session.contexts_by_entry {
            catalogs.insert(entry.clone(), context.cgs.catalog_cgs_hash_hex());
        }
        if let Some(captured) = symbol_context.captured() {
            catalogs = captured.catalog_fingerprints().clone();
        }
        let revision = session.core.tip_seq().await.0;
        let mut recorded = std::collections::BTreeSet::new();
        for reference in &output.code_plan_run_artifacts {
            if !recorded.insert(reference.run_id.as_str()) {
                continue;
            }
            let id = crate::run_artifacts::RunArtifactId::from_wire(&reference.run_id)
                .ok_or_else(|| LearningError::Evidence("invalid run identifier".into()))?;
            let payload = host
                .run_artifacts
                .get_payload_result(&session.prompt_hash, execute_session, id)
                .await
                .map_err(|e| LearningError::ArtifactRetrieval(Box::new(e)))?
                .ok_or_else(|| LearningError::Evidence("run evidence missing".into()))?;
            let document = crate::run_artifacts::parse_run_artifact_document_bytes(&payload.bytes)
                .map_err(|e| LearningError::ArtifactDecoding(Box::new(e)))?;
            if document.prompt_hash != session.prompt_hash
                || document.session_id != execute_session
                || document.run_id != reference.run_id
                || document
                    .recorded_collection()
                    .map_err(|e| LearningError::CollectionRestoration(Box::new(e)))?
                    .coverage()
                    != plasm_runtime::ResultCoverage::Complete
            {
                return Err(LearningError::Evidence(
                    "run evidence incomplete or belongs to another session".into(),
                ));
            }
            repository
                .record_execution(&ExecutionReceipt {
                    scope: scope.clone(),
                    logical_session,
                    session_revision: revision,
                    run_id: id,
                    plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
                    prompt_hash: session.prompt_hash.parse()?,
                    execute_session: execute_session.parse()?,
                    run_artifact: serde_json::to_value(document)?,
                    source: source.into(),
                    compiled_plan: serde_json::to_value(&bundle.artifact().comp)?,
                    catalog_fingerprints: catalogs.clone(),
                    observation: observation.clone(),
                    catalogue_requirements: catalogue_requirements.clone(),
                    symbol_context: symbol_context.clone(),
                })
                .await?;
        }
        Ok(())
    }
}

pub(crate) fn learning_tenant(tenant: &str) -> String {
    if tenant.is_empty() {
        "local-appliance".into()
    } else {
        format!("tenant:{tenant}")
    }
}

pub(crate) fn scope(
    tenant: &str,
    config: Option<&crate::mcp_runtime_config::McpRuntimeConfig>,
) -> LibraryScope {
    LibraryScope {
        tenant: learning_tenant(tenant),
        project: config
            .map(|c| {
                serde_json::to_string(&(&c.workspace_slug, &c.project_slug))
                    .expect("scope strings serialize")
            })
            .unwrap_or_else(|| "local-appliance".into()),
    }
}

#[cfg(test)]
mod operation_tests;
