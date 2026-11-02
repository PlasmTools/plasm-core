//! Native adapters use the same execution journal, review fence and example compiler.
pub use super::execution::observe_execution;
use super::{
    model::{AchievedSubtask, LibraryScope},
    LearningMode, WorkflowLearning,
};
use crate::{
    execute_session::{ExecuteSession, SessionReuseKey},
    intent_provenance::IntentProvenance,
    server_state::PlasmHostState,
    session_identity::LogicalSessionId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NativeLearningMode {
    #[default]
    Baseline,
    Collect,
    Admission,
    Examples,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLibraryConfig {
    pub scope: LibraryScope,
    #[serde(rename = "databaseUrl")]
    pub database_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeLearningConfig {
    #[default]
    Baseline,
    Collect(NativeLibraryConfig),
    Admission(NativeLibraryConfig),
    Examples(NativeLibraryConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum NativeLearningError {
    #[error(transparent)]
    RunIdentity(#[from] crate::run_artifacts::RunArtifactWireError),
    #[error("invalid workflow learning configuration: {0}")]
    Configuration(&'static str),
    #[error("workflow operation is unavailable in this mode")]
    Mode,
    #[error("workflow run evidence is unavailable in this session")]
    EvidenceUnavailable,
    #[error(transparent)]
    Repository(#[from] super::repository::RepositoryError),
    #[error(transparent)]
    Learning(#[from] super::LearningError),
    #[error(transparent)]
    Review(#[from] super::review::ReviewError),
    #[error(transparent)]
    Persistence(
        #[from] crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError,
    ),
    #[error(transparent)]
    Registration(#[from] crate::session_identity::LogicalSessionRegistrationError),
    #[error(transparent)]
    Intent(#[from] crate::intent_provenance::IntentProvenanceError),
    #[error(transparent)]
    Identity(#[from] uuid::Error),
    #[error(transparent)]
    Serving(#[from] super::serving::ServingError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl NativeLearningConfig {
    pub fn mode(&self) -> NativeLearningMode {
        match self {
            Self::Baseline => NativeLearningMode::Baseline,
            Self::Collect(_) => NativeLearningMode::Collect,
            Self::Admission(_) => NativeLearningMode::Admission,
            Self::Examples(_) => NativeLearningMode::Examples,
        }
    }
    pub fn library(&self) -> Option<&NativeLibraryConfig> {
        match self {
            Self::Baseline => None,
            Self::Collect(c) | Self::Admission(c) | Self::Examples(c) => Some(c),
        }
    }
    pub fn scope(&self) -> Result<LibraryScope, NativeLearningError> {
        self.library()
            .map(|c| LibraryScope {
                tenant: super::learning_tenant(&c.scope.tenant),
                project: c.scope.project.clone(),
            })
            .ok_or(NativeLearningError::Mode)
    }
    pub fn validate(&self) -> Result<(), NativeLearningError> {
        if let Some(config) = self.library() {
            if config.scope.tenant.trim().is_empty()
                || config.scope.project.trim().is_empty()
                || config.database_url.trim().is_empty()
            {
                return Err(NativeLearningError::Configuration(
                    "enabled modes require tenant, project and databaseUrl",
                ));
            }
        }
        Ok(())
    }
    pub fn learning(&self) -> Arc<WorkflowLearning> {
        Arc::new(WorkflowLearning::configured(
            match self.mode() {
                NativeLearningMode::Baseline => LearningMode::Disabled,
                NativeLearningMode::Collect => LearningMode::Shadow,
                NativeLearningMode::Admission | NativeLearningMode::Examples => {
                    LearningMode::Serving
                }
            },
            self.library().map(|c| c.database_url.clone()),
        ))
    }

    /// Publishes a native exposure through the core descriptor CAS without opening the learning database.
    pub async fn publish_exposure(
        &self,
        host: &PlasmHostState,
        session: &ExecuteSession,
        logical_id: &str,
        session_id: &str,
        turns: &[String],
    ) -> Result<(), NativeLearningError> {
        if self.mode() == NativeLearningMode::Baseline {
            return Ok(());
        }
        let logical = LogicalSessionId::parse(logical_id)?;
        let provenance = IntentProvenance::from_turns(turns.to_vec())?;
        let coordinate = crate::session_coordination::ExecuteCoordKey {
            prompt_hash: session.prompt_hash.clone(),
            session_id: session_id.to_owned(),
        };
        host.session_coordination
            .with_exposure_commit(&coordinate, || async {
                host.logical_sessions
                    .validate_routed_session(logical, &session.tenant_scope, &provenance)
                    .await?;
                let key = SessionReuseKey {
                    tenant_scope: session.tenant_scope.clone(),
                    entry_id: session.entry_id.clone(),
                    catalog_cgs_hash: session.catalog_cgs_hash.clone(),
                    entities: session.entities.clone(),
                    context_intent: session.context_intent.clone(),
                    principal: None,
                    logical_session_id: Some(logical_id.to_owned()),
                };
                if host
                    .sessions
                    .get_by_strs(&session.prompt_hash, session_id)
                    .await
                    .is_some()
                {
                    host.replace_execute_session(&session.prompt_hash, session_id, session.clone())
                        .await?;
                } else {
                    host.store_execute_session(
                        key,
                        session.prompt_hash.clone(),
                        session_id.to_owned(),
                        session.clone(),
                    )
                    .await?;
                }
                host.logical_sessions
                    .register_routed_session(
                        logical,
                        &session.tenant_scope,
                        &provenance,
                        session.discovery_pin.clone(),
                    )
                    .await?;
                host.logical_execute_bindings
                    .insert(
                        logical.as_uuid(),
                        session.prompt_hash.clone(),
                        session_id.to_owned(),
                    )
                    .await;
                Ok(())
            })
            .await
    }

    /// Registers the actual native session, never a reconstructed transcript.
    pub async fn register(
        &self,
        host: &PlasmHostState,
        session: &ExecuteSession,
        logical_id: &str,
        session_id: &str,
        turns: &[String],
    ) -> Result<(), NativeLearningError> {
        self.publish_exposure(host, session, logical_id, session_id, turns)
            .await?;
        if self.mode() == NativeLearningMode::Collect {
            host.workflow_learning
                .repository()
                .await?
                .bind_session(&self.scope()?, LogicalSessionId::parse(logical_id)?)
                .await?;
        }
        Ok(())
    }

    pub async fn completed_runs(
        &self,
        host: &PlasmHostState,
        logical: &str,
        intent: &str,
    ) -> Result<serde_json::Value, NativeLearningError> {
        if self.mode() != NativeLearningMode::Collect {
            return Err(NativeLearningError::Mode);
        }
        let runs = host
            .workflow_learning
            .repository()
            .await?
            .execution_index(&self.scope()?, LogicalSessionId::parse(logical)?)
            .await?;
        Ok(serde_json::json!({"intent":intent,"runs":runs}))
    }

    pub async fn read_evidence(
        &self,
        host: &PlasmHostState,
        logical: &str,
        run_id: &str,
    ) -> Result<serde_json::Value, NativeLearningError> {
        if self.mode() != NativeLearningMode::Collect {
            return Err(NativeLearningError::Mode);
        }
        let receipt = host
            .workflow_learning
            .repository()
            .await?
            .execution_receipt(
                &self.scope()?,
                LogicalSessionId::parse(logical)?,
                run_id.parse::<crate::run_artifacts::RunArtifactWire>()?.0,
            )
            .await?;
        let value = serde_json::to_value(receipt)?;
        if serde_json::to_vec(&value)?.len() > 1_048_576 {
            return Err(NativeLearningError::Configuration(
                "immutable receipt exceeds the 1 MiB evidence read limit",
            ));
        }
        Ok(value)
    }

    pub async fn review(
        &self,
        host: &PlasmHostState,
        session: &ExecuteSession,
        logical: &str,
        execute_session: &str,
        subtasks: Vec<AchievedSubtask>,
    ) -> Result<serde_json::Value, NativeLearningError> {
        if self.mode() != NativeLearningMode::Collect {
            return Err(NativeLearningError::Mode);
        }
        let handle = host
            .workflow_learning
            .seal_review(
                host,
                super::ReviewSession {
                    scope: self.scope()?.clone(),
                    logical_session: LogicalSessionId::parse(logical)?,
                    prompt_hash: session.prompt_hash.clone(),
                    execute_session: execute_session.to_owned(),
                    subtasks,
                },
            )
            .await?;
        Ok(serde_json::json!({"review_ref":handle.id.to_string(),"digest":handle.digest}))
    }

    pub async fn fetch(
        &self,
        host: &PlasmHostState,
        session: &ExecuteSession,
        execute_session: &str,
        revision: &str,
        allowed: &crate::discovery_store::DiscoveryAuthorization,
    ) -> Result<super::serving::ServedExample, NativeLearningError> {
        if self.mode() != NativeLearningMode::Examples {
            return Err(NativeLearningError::Mode);
        }
        let repository = host.workflow_learning.repository().await?;
        let prepared = super::serving::prepare_example_fetch(
            host,
            &repository,
            self.scope()?.clone(),
            uuid::Uuid::parse_str(revision)?,
            &session.prompt_hash,
            execute_session,
            allowed,
        )
        .await?;
        Ok(super::serving::commit_example_fetch(host, &repository, prepared, allowed).await?)
    }
}
