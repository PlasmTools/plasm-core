//! Verified workflow learning. Provider proposals never bypass deterministic gates.
pub mod archive;
pub mod catalogue_guidance;
pub mod deferred_recall;
pub(crate) mod execution;
pub mod filesystem_archive;
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
pub(crate) use review::{ReviewError, ReviewSession};
pub mod repository;
pub use repository::training;
pub mod applicability;
pub mod review_packet;
pub mod serving;
mod serving_projection;
pub mod teaching;
mod worker;
pub use worker::RecallStage;

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::PlasmPlanRunResult;
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
    pub execution_id: &'a str,
    pub training_permit: &'a repository::training::TrainingEpochPermit,
}
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearningMode {
    Disabled,
    Shadow,
    ShadowWithRecall,
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
    #[error("teaching applicability: {0}")]
    Applicability(#[source] applicability::Error),
    #[error("teaching recall timed out during {0}")]
    RecallTimeout(RecallStage),
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
}

/// Lazy initialization keeps optional learning failures out of engine startup.
pub struct WorkflowLearning {
    pub mode: LearningMode,
    database_url: Option<String>,
    repository: OnceCell<Arc<WorkflowRepository>>,
    worker: std::sync::atomic::AtomicBool,
    pub deferred_recalls: Arc<deferred_recall::DeferredRecalls>,
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
            deferred_recalls: Arc::default(),
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
            deferred_recalls: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_test_repository(mode: LearningMode, repository: Arc<WorkflowRepository>) -> Self {
        Self {
            mode,
            database_url: None,
            repository: OnceCell::new_with(Some(repository)),
            worker: false.into(),
            deferred_recalls: Arc::default(),
        }
    }
    pub async fn begin_execution(
        &self,
        tenant: &str,
        logical: LogicalSessionId,
        id: &str,
    ) -> Result<Option<(LibraryScope, repository::training::TrainingEpochPermit)>, LearningError>
    {
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
        Ok(repository
            .begin_execution(&scope, logical, id)
            .await?
            .map(|permit| (scope, permit)))
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

    /// Open the existing ledger without applying migrations. The caller may
    /// provide SELECT-only credentials; this method never initializes the
    /// mutable repository or performs DDL.
    pub async fn readonly_repository(&self) -> Result<WorkflowRepository, LearningError> {
        if !self.enabled() {
            return Err(LearningError::Disabled);
        }
        let url = self
            .database_url
            .as_deref()
            .ok_or(LearningError::Disabled)?;
        let repository = WorkflowRepository::connect(url).await?;
        repository.validate_current_schema().await?;
        Ok(repository)
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
        completed: CompletedExecution<'_>,
    ) -> Result<(), LearningError> {
        let CompletedExecution {
            session,
            logical_session,
            execute_session,
            source,
            bundle,
            output,
            training_permit,
            execution_id,
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
        if !matches!(
            output.agent_outcome,
            crate::plasm_plan_run::PlanAgentOutcome::Ready
        ) {
            return Err(LearningError::Evidence(
                "execution did not complete successfully".into(),
            ));
        }
        let teaching_plan = review_packet::TeachingPlan::capture(session, bundle).await;
        let mut catalogs = BTreeMap::new();
        for (entry, context) in &session.contexts_by_entry {
            catalogs.insert(entry.clone(), context.cgs.catalog_cgs_hash_hex());
        }
        if let Some(captured) = bundle.compilation_symbols().captured() {
            catalogs = captured.catalog_fingerprints().clone();
        }
        let revision = session.core.tip_seq().await.0;
        let attempt = uuid::Uuid::parse_str(execution_id)
            .map_err(|_| LearningError::Evidence("invalid execution identity".into()))?;
        let receipt = ExecutionReceipt {
            scope: scope.clone(),
            logical_session,
            session_revision: revision,
            execution_id: model::ExecutionId::from_attempt(attempt),
            plan_id: CompiledProgramId::from_comp(&bundle.artifact().comp),
            prompt_hash: session.prompt_hash.parse()?,
            execute_session: execute_session.parse()?,
            source: source.into(),
            compiled_plan: serde_json::to_value(&bundle.artifact().comp)?,
            teaching_plan,
            catalog_fingerprints: catalogs,
            observation,
            catalogue_requirements,
        };
        repository
            .record_execution_batch(&[receipt], training_permit, Some(execution_id))
            .await?;
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
