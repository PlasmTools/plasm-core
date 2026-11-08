#![recursion_limit = "512"]

mod effect_obligations;
mod engine;
mod tracing_setup;
mod transport;
mod types;
mod workflow_verification;

pub use engine::{
    AgentEngine, CapabilityIntrospection, CatalogInfo, CatalogIntrospection, DryRunResult,
    EntityIntrospection, RunPlanResult, TeachingExposureResult,
};
pub use types::{JsTransportRequest, JsTransportResponse};

use engine::AgentEngine as InnerEngine;
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Wire-schema parity gate, using the same parser as MCP and native execution.
#[napi]
pub fn is_executable_run_reference(raw: String) -> bool {
    plasm_core::run_reference::ExecutableRunTarget::is_canonical(&raw)
}
use plasm_agent_core::discovery_service::{DiscoveryService, RouteTurn};
use plasm_agent_core::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, PreparedCatalog,
};
use plasm_agent_core::http_execute::CapabilitySeed;
use plasm_agent_core::workflow_learning::native::{NativeLearningConfig, NativeLearningMode};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::transport::{JsCallbackHttpTransport, JsHostTransport};

#[napi(object)]
pub struct JsCatalogInfo {
    pub entry_id: String,
    pub catalog_cgs_hash: String,
}

#[napi(object)]
pub struct JsSeed {
    pub api: String,
    pub entity: String,
}

#[napi(object)]
pub struct JsTeachingResult {
    pub prompt: String,
    pub declarations_changed: bool,
    pub delta_refs: Vec<String>,
}

#[napi(object)]
pub struct JsDryRunResult {
    pub write_count: u32,
    pub failure_json: Option<String>,
    pub plan_commit_ref: String,
    pub summary: String,
    pub comp_json: String,
    pub fused_clean_read: bool,
}

#[napi(object)]
pub struct JsRunPlanResult {
    pub ok: bool,
    pub message: String,
    #[napi(js_name = "rowsJson")]
    pub rows_json: Option<String>,
    #[napi(js_name = "metaJson")]
    pub meta_json: Option<String>,
    #[napi(js_name = "artifactsJson")]
    pub artifacts_json: Option<String>,
    #[napi(js_name = "failureJson")]
    pub failure_json: Option<String>,
    #[napi(js_name = "continuationRef")]
    pub continuation_ref: Option<String>,
}

#[derive(Debug, Error)]
enum NodeEngineError {
    #[error("Plasm engine is closing or closed")]
    Closed,
    #[error("logical session `{id}` is unknown")]
    UnknownSession { id: String },
    #[error("logical session has no discovery generation pin")]
    SessionPinMissing,
    #[error("no discovery generation has been activated")]
    NoActivatedGeneration,
    #[error("discovery store is unavailable")]
    DiscoveryStoreUnavailable,
    #[error("discovery must be activated before routing")]
    DiscoveryNotActivated,
    #[error("routing receipt does not match the logical session pin")]
    RoutingPinMismatch,
    #[error("discovery database URL is not configured")]
    DiscoveryDatabaseUrlMissing,
    #[error("discovery store operation failed")]
    DiscoveryStore(#[from] plasm_agent_core::discovery_store::DiscoveryStoreError),
    #[error("discovery routing failed")]
    DiscoveryService(#[from] plasm_agent_core::discovery_service::DiscoveryServiceError),
    #[error("deployment bindings JSON is invalid")]
    DeploymentBindingsJson(#[from] serde_json::Error),
}

type NodeResult<T> = std::result::Result<T, NodeEngineError>;

fn map_err(err: impl std::error::Error) -> Error {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    Error::from_reason(message)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// In-process Plasm engine for NAPI.
///
/// Session engines serialize admission and plan preparation. Reviewed live
/// runs own their execution state and do not retain the session engine lock
/// while awaiting transport callbacks.
#[napi]
pub struct PlasmEngine {
    closing: std::sync::atomic::AtomicBool,
    workflow_config: OnceCell<NativeLearningConfig>,
    workflow_owner: OnceCell<Arc<plasm_agent_core::workflow_learning::WorkflowLearning>>,
    training_actor: OnceCell<plasm_agent_core::workflow_learning::training::TrustedTrainingActor>,
    drained: Arc<tokio::sync::RwLock<bool>>,
    learning_pool: Arc<plasm_agent_core::live_plan_run_worker::LivePlanRunPool>,
    python_pool: Arc<plasm_agent_core::python_pool::PythonPool>,
    inner: Arc<AsyncMutex<InnerEngine>>,
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<InnerEngine>>>>,
    session_admission: Mutex<HashMap<String, Arc<SessionAdmission>>>,
    effect_obligations: effect_obligations::EffectObligations,
    effect_controls: Arc<
        Mutex<
            HashMap<
                String,
                (
                    String,
                    plasm_runtime::execution::effect_continuation::EffectContinuation,
                ),
            >,
        >,
    >,
    effect_pauses: Mutex<HashMap<String, PendingEffectEntry>>,
    discovery_store: Arc<OnceCell<DiscoveryStore>>,
    activated: tokio::sync::RwLock<Option<(String, std::collections::BTreeSet<String>)>>,
}

type EffectWorker = plasm_agent_core::live_plan_run_worker::LivePlanRunHandle<
    std::result::Result<engine::RunPlanResult, engine::AgentEngineError>,
    std::convert::Infallible,
>;

struct PendingEffect {
    workflow: String,
    reference: String,
    control: plasm_runtime::execution::effect_continuation::EffectContinuation,
    pause: Option<plasm_runtime::execution::effect_continuation::EffectPause>,
    pauses: plasm_runtime::execution::effect_continuation::EffectPauseReceiver,
    worker: EffectWorker,
}

#[derive(Clone)]
struct PendingEffectEntry {
    workflow: String,
    deadline: std::time::Instant,
    control: plasm_runtime::execution::effect_continuation::EffectContinuation,
    pending: Arc<AsyncMutex<PendingEffect>>,
}

fn prune_expired_effects(entries: &mut HashMap<String, PendingEffectEntry>) {
    let now = std::time::Instant::now();
    entries.retain(|_, entry| {
        if now >= entry.deadline {
            entry.control.cancel();
            false
        } else {
            true
        }
    });
}

struct ActiveEffectGuard {
    reference: String,
    controls: Arc<
        Mutex<
            HashMap<
                String,
                (
                    String,
                    plasm_runtime::execution::effect_continuation::EffectContinuation,
                ),
            >,
        >,
    >,
}

impl Drop for ActiveEffectGuard {
    fn drop(&mut self) {
        lock(&self.controls).remove(&self.reference);
    }
}

struct SessionAdmission {
    sealed: std::sync::atomic::AtomicBool,
    in_flight: Arc<tokio::sync::RwLock<()>>,
    discovery: Arc<tokio::sync::Mutex<()>>,
    clock: Option<String>,
}

impl SessionAdmission {
    fn new(clock: Option<String>) -> Self {
        Self {
            sealed: std::sync::atomic::AtomicBool::new(false),
            in_flight: Arc::new(tokio::sync::RwLock::new(())),
            discovery: Arc::new(tokio::sync::Mutex::new(())),
            clock,
        }
    }
}

struct WorkflowAdmin {
    guard: tokio::sync::OwnedRwLockReadGuard<bool>,
    scope: plasm_agent_core::workflow_learning::model::LibraryScope,
    owner: Arc<plasm_agent_core::workflow_learning::WorkflowLearning>,
}

impl Default for PlasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlasmEngine {
    fn cancel_effect_continuations(&self, workflow: Option<&str>) {
        for (owner, control) in lock(&self.effect_controls).values() {
            if workflow.is_none_or(|workflow| workflow == owner) {
                control.cancel();
            }
        }
    }
    pub fn new() -> Self {
        Self::with_runtime(None)
    }
    async fn admit(&self) -> Result<tokio::sync::OwnedRwLockReadGuard<bool>> {
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        let guard = Arc::clone(&self.drained).read_owned().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        Ok(guard)
    }

    async fn admit_training(&self) -> Result<tokio::sync::OwnedRwLockReadGuard<bool>> {
        let guard = Arc::clone(&self.drained).read_owned().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        Ok(guard)
    }

    fn workflow_error(&self, error: impl std::error::Error) -> Error {
        let mut error = map_err(error);
        if let Some(config) = self
            .workflow_config
            .get()
            .and_then(NativeLearningConfig::library)
        {
            error.reason = error
                .reason
                .replace(&config.database_url, "[workflow database]");
        }
        error
    }

    async fn administrative_generation(
        &self,
    ) -> Result<workflow_verification::AdministrativeGeneration> {
        let (generation, allowed) = self
            .activated
            .read()
            .await
            .clone()
            .ok_or_else(|| map_err(NodeEngineError::NoActivatedGeneration))?;
        let store = self
            .discovery_store
            .get()
            .ok_or_else(|| map_err(NodeEngineError::DiscoveryStoreUnavailable))?;
        let (catalogs, compiled, deployments) = store
            .load_generation(&generation)
            .await
            .map_err(NodeEngineError::from)
            .map_err(map_err)?;
        let pin = DiscoverySessionPin {
            generation,
            authorization: DiscoveryAuthorization::catalogs(allowed),
            pin_id: plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string(),
        };
        Ok(workflow_verification::AdministrativeGeneration {
            catalogs,
            compiled,
            pin,
            deployments,
        })
    }

    async fn workflow_admin(&self) -> Result<WorkflowAdmin> {
        let guard = Arc::clone(&self.drained).read_owned().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        let config = self
            .workflow_config
            .get()
            .ok_or_else(|| Error::from_reason("workflow learning is not configured"))?;
        if config.mode() != NativeLearningMode::Collect {
            return Err(Error::from_reason(
                "corpus administration requires collect mode",
            ));
        }
        Ok(WorkflowAdmin {
            guard,
            scope: config.scope().map_err(map_err)?,
            owner: Arc::clone(
                self.workflow_owner
                    .get()
                    .ok_or_else(|| Error::from_reason("workflow learning is not configured"))?,
            ),
        })
    }

    fn session_engine(&self, id: &str) -> NodeResult<Arc<AsyncMutex<InnerEngine>>> {
        lock(&self.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| NodeEngineError::UnknownSession { id: id.to_owned() })
    }

    async fn admit_session(
        &self,
        id: Option<&str>,
    ) -> Result<Option<tokio::sync::OwnedRwLockReadGuard<()>>> {
        let Some(id) = id else { return Ok(None) };
        let gate = lock(&self.session_admission)
            .get(id)
            .cloned()
            .ok_or_else(|| map_err(NodeEngineError::UnknownSession { id: id.to_owned() }))?;
        if gate.sealed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        let guard = Arc::clone(&gate.in_flight).read_owned().await;
        if gate.sealed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(map_err(NodeEngineError::Closed));
        }
        Ok(Some(guard))
    }

    async fn admit_discovery(
        &self,
        id: Option<&str>,
    ) -> Result<Option<tokio::sync::OwnedMutexGuard<()>>> {
        let Some(id) = id else { return Ok(None) };
        let gate = lock(&self.session_admission)
            .get(id)
            .cloned()
            .ok_or_else(|| map_err(NodeEngineError::UnknownSession { id: id.to_owned() }))?;
        Ok(Some(Arc::clone(&gate.discovery).lock_owned().await))
    }

    async fn execution_engine(
        &self,
        logical_session_id: Option<&str>,
    ) -> Result<tokio::sync::OwnedMutexGuard<InnerEngine>> {
        let shared = match logical_session_id {
            Some(id) => self.session_engine(id).map_err(map_err)?,
            None => Arc::clone(&self.inner),
        };
        let engine = shared.lock_owned().await;
        if logical_session_id.is_some() {
            self.refresh_discovery_session(&engine)
                .await
                .map_err(map_err)?;
        }
        Ok(engine)
    }

    async fn routing_inputs(
        &self,
        logical_session_id: Option<&str>,
    ) -> NodeResult<(
        String,
        DiscoveryAuthorization,
        Vec<plasm_core::prerequisites::CapabilityRef>,
    )> {
        if let Some(id) = logical_session_id {
            let shared = self.session_engine(id)?;
            let engine = shared.lock().await;
            let pin = engine
                .discovery_pin()
                .ok_or_else(|| NodeEngineError::SessionPinMissing)?;
            Ok((
                pin.generation.clone(),
                pin.authorization.clone(),
                engine.exposed_capabilities(),
            ))
        } else {
            let (generation, allowed) = self
                .activated
                .read()
                .await
                .clone()
                .ok_or(NodeEngineError::NoActivatedGeneration)?;
            Ok((
                generation,
                DiscoveryAuthorization::catalogs(allowed),
                Vec::new(),
            ))
        }
    }

    async fn refresh_discovery_session(&self, engine: &InnerEngine) -> NodeResult<()> {
        let pin = engine
            .discovery_pin()
            .cloned()
            .ok_or(NodeEngineError::SessionPinMissing)?;
        let store = self
            .discovery_store
            .get()
            .ok_or(NodeEngineError::DiscoveryStoreUnavailable)?;
        store
            .refresh_session_pin(&pin)
            .await
            .map_err(NodeEngineError::from)
    }
}

#[napi]
impl PlasmEngine {
    #[napi(constructor)]
    pub fn with_runtime(monty_binary: Option<String>) -> Self {
        tracing_setup::init();
        let python_pool = Arc::new(match monty_binary {
            Some(path) => plasm_agent_core::python_pool::PythonPool::with_binary(path.into()),
            None => Default::default(),
        });
        Self {
            closing: std::sync::atomic::AtomicBool::new(false),
            workflow_config: OnceCell::new(),
            workflow_owner: OnceCell::new(),
            training_actor: OnceCell::new(),
            drained: Arc::new(tokio::sync::RwLock::new(false)),
            learning_pool: Arc::new(plasm_agent_core::live_plan_run_worker::LivePlanRunPool::new()),
            inner: Arc::new(AsyncMutex::new(InnerEngine::new(python_pool.clone()))),
            python_pool,
            sessions: Mutex::new(HashMap::new()),
            session_admission: Mutex::new(HashMap::new()),
            effect_obligations: Default::default(),
            effect_controls: Default::default(),
            effect_pauses: Default::default(),
            discovery_store: Arc::new(OnceCell::new()),
            activated: Default::default(),
        }
    }

    #[napi]
    pub async fn configure_workflow_learning(&self, config_json: String) -> Result<()> {
        let _exclusive = self.drained.write().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire)
            || self.workflow_config.get().is_some()
            || !lock(&self.sessions).is_empty()
        {
            return Err(Error::from_reason(
                "workflow configuration must precede session creation and cannot be changed",
            ));
        }
        let config: NativeLearningConfig = serde_json::from_str(&config_json).map_err(map_err)?;
        config.validate().map_err(map_err)?;
        self.inner
            .lock()
            .await
            .configure_workflow_learning(config.clone())
            .map_err(map_err)?;
        self.workflow_owner
            .set(config.learning())
            .map_err(|_| Error::from_reason("workflow configuration already set"))?;
        self.workflow_config
            .set(config)
            .map_err(|_| Error::from_reason("workflow configuration already set"))?;
        Ok(())
    }

    /// Trusted embedding-host delegation. This is never registered as an agent tool.
    /// Installing a grant does not opt any logical session into training.
    #[napi]
    pub async fn configure_training(&self, grant_json: String) -> Result<()> {
        use plasm_agent_core::workflow_learning::training::{
            TrainingGrantSpec, TrustedTrainingActor,
        };
        let _exclusive = self.drained.write().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire)
            || self.training_actor.get().is_some()
        {
            return Err(Error::from_reason(
                "training delegation is unavailable or already configured",
            ));
        }
        let config = self.workflow_config.get().ok_or_else(|| {
            Error::from_reason("configure the workflow library before training delegation")
        })?;
        if config.mode() == NativeLearningMode::Baseline {
            return Err(Error::from_reason(
                "training requires an enabled workflow library",
            ));
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TrainingHost {
            subject: String,
            credential: String,
            grant: Option<TrainingGrantSpec>,
            idempotency_key: Option<String>,
        }
        let input: TrainingHost = serde_json::from_str(&grant_json).map_err(map_err)?;
        let scope = config.scope().map_err(map_err)?;
        let issuer = TrustedTrainingActor::trusted_host(
            scope.clone(),
            "embedded-host".into(),
            "native-host".into(),
        )
        .map_err(map_err)?;
        let actor = TrustedTrainingActor::authenticated(scope, input.subject, input.credential)
            .map_err(map_err)?;
        if let Some(grant) = input.grant {
            if grant.grantee != actor.subject() || grant.credential != actor.credential() {
                return Err(Error::from_reason(
                    "training delegation must belong to the configured host actor",
                ));
            }
            let key = input
                .idempotency_key
                .ok_or_else(|| Error::from_reason("training grant requires an idempotency key"))?
                .parse()
                .map_err(map_err)?;
            let owner = self
                .workflow_owner
                .get()
                .cloned()
                .ok_or_else(|| Error::from_reason("workflow library unavailable"))?;
            self.learning_pool
                .run_local(move || async move {
                    let repository = owner.repository().await.map_err(
                        plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                    )?;
                    repository
                        .install_training_grant(&issuer, key, grant)
                        .await
                        .map_err(
                            plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                        )
                })
                .await
                .map_err(map_err)?
                .map_err(|error| self.workflow_error(error))?;
        }
        self.training_actor
            .set(actor)
            .map_err(|_| Error::from_reason("training delegation already configured"))?;
        Ok(())
    }

    #[napi]
    pub async fn training_status(&self, logical_session_id: Option<String>) -> Result<String> {
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.training_status().await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    /// Host-only evaluation classification; immutable and independent of model input.
    #[napi]
    pub async fn seal_training_evaluation(&self, logical_session_id: Option<String>) -> Result<()> {
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.seal_training_evaluation().await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    #[napi]
    pub async fn training_command(
        &self,
        command_json: String,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        let actor = self
            .training_actor
            .get()
            .cloned()
            .ok_or_else(|| Error::from_reason("training delegation is not configured"))?;
        let command = serde_json::from_str(&command_json).map_err(map_err)?;
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.training_command(&actor, &command).await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    #[napi]
    pub async fn training_candidates(
        &self,
        command_json: String,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        let actor = self
            .training_actor
            .get()
            .cloned()
            .ok_or_else(|| Error::from_reason("training delegation is not configured"))?;
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.training_candidates(&actor, &command_json).await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    /// Stop solving admission for one logical session and wait for its native callbacks.
    #[napi]
    pub async fn drain_session(&self, logical_session_id: String) -> Result<()> {
        let _admission = self.admit_training().await?;
        let gate = lock(&self.session_admission)
            .get(&logical_session_id)
            .cloned()
            .ok_or_else(|| {
                map_err(NodeEngineError::UnknownSession {
                    id: logical_session_id.clone(),
                })
            })?;
        gate.sealed
            .store(true, std::sync::atomic::Ordering::Release);
        self.cancel_effect_continuations(Some(&logical_session_id));
        lock(&self.effect_pauses).retain(|_, entry| entry.workflow != logical_session_id);
        let _exclusive = gate.in_flight.write().await;
        Ok(())
    }

    /// Release a drained session without affecting other sessions on this host.
    #[napi]
    pub async fn release_session(&self, logical_session_id: String) -> Result<()> {
        self.drain_session(logical_session_id.clone()).await?;
        let mut sessions = lock(&self.sessions);
        sessions.remove(&logical_session_id);
        lock(&self.session_admission).remove(&logical_session_id);
        self.effect_obligations.release(&logical_session_id);
        Ok(())
    }

    #[napi]
    pub async fn completed_workflow_executions(
        &self,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        if self.training_actor.get().is_none() {
            return Err(Error::from_reason("training identity is not configured"));
        }
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.completed_workflow_executions().await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    #[napi]
    pub async fn read_teaching_program(
        &self,
        execution_id: String,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        if self.training_actor.get().is_none() {
            return Err(Error::from_reason("training identity is not configured"));
        }
        let _admission = self.admit_training().await?;
        let engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.read_teaching_program(&execution_id).await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    #[napi]
    pub async fn review_workflow(
        &self,
        request_json: String,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        let actor = self
            .training_actor
            .get()
            .cloned()
            .ok_or_else(|| Error::from_reason("training delegation is not configured"))?;
        let _admission = self.admit_training().await?;
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                engine.review_workflow(&request_json, actor).await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    #[napi]
    pub async fn fetch_workflow_example(
        &self,
        revision_ref: String,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        self.learning_pool
            .run_local(move || async move {
                let _admission = _admission;
                let _session_admission = _session_admission;
                engine.fetch_workflow_example(&revision_ref).await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    /// Host-only corpus administration; never exposed by the agent review port.
    #[napi]
    pub async fn run_workflow_promotion(&self, timeout_seconds: u32) -> Result<String> {
        let admin = self.workflow_admin().await?;
        let status = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                plasm_agent_core::workflow_learning::native_admin::drain_promotion(
                    &admin.owner,
                    &admin.scope,
                    std::time::Duration::from_secs(u64::from(timeout_seconds)),
                )
                .await
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&status).map_err(map_err)
    }

    #[napi]
    pub async fn export_workflow_proposals(&self) -> Result<String> {
        let admin = self.workflow_admin().await?;
        let artifacts = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.repository().await?;
                Ok::<_, plasm_agent_core::workflow_learning::LearningError>(
                    repository.export_review_artifacts(&admin.scope).await?,
                )
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&artifacts).map_err(map_err)
    }

    #[napi]
    pub async fn publish_frozen_workflow_review(&self, request_json: String) -> Result<String> {
        let request: plasm_agent_core::workflow_learning::repository::FrozenPublicationRequest =
            serde_json::from_str(&request_json).map_err(map_err)?;
        let admin = self.workflow_admin().await?;
        let actor = self.training_actor.get().cloned().ok_or_else(|| {
            Error::from_reason("publication requires configured trusted authority")
        })?;
        let receipt = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.repository().await?;
                Ok::<_, plasm_agent_core::workflow_learning::LearningError>(
                    repository
                        .publish_frozen(
                            &admin.scope,
                            request.review_ref,
                            &request.artifact_hashes,
                            &request.catalogue_guidance_digest,
                            &actor,
                        )
                        .await?,
                )
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&receipt).map_err(map_err)
    }

    /// Export a scoped teaching archive directly to a new filesystem directory.
    #[napi]
    pub async fn export_workflow_archive(
        &self,
        kind: String,
        destination: String,
    ) -> Result<String> {
        use plasm_agent_core::workflow_learning::archive::ArchiveKind;
        let kind = match kind.as_str() {
            "private_collection" => ArchiveKind::PrivateCollection,
            "serving_library" => ArchiveKind::ServingLibrary,
            _ => return Err(Error::from_reason("invalid archive kind")),
        };
        let admin = self.workflow_admin().await?;
        let manifest = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.readonly_repository().await?;
                let manifest = repository
                    .export_filesystem_archive(
                        &admin.scope,
                        std::path::Path::new(&destination),
                        kind,
                    )
                    .await?;
                Ok::<_, plasm_agent_core::workflow_learning::native::NativeLearningError>(manifest)
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&manifest).map_err(map_err)
    }

    /// Record an exact-review external decision under configured host authority.
    /// This is an administrative interface, never an agent-callable tool.
    #[napi]
    pub async fn authorize_imported_workflow_review(&self, request_json: String) -> Result<()> {
        use plasm_agent_core::workflow_learning::filesystem_archive::ImportedReviewAttestationRequest;
        let request = parse_archive_metadata::<ImportedReviewAttestationRequest>(&request_json)?;
        let admin = self.workflow_admin().await?;
        let actor = self.training_actor.get().cloned().ok_or_else(|| {
            Error::from_reason("review attestation requires configured trusted authority")
        })?;
        if actor.scope() != &admin.scope {
            return Err(Error::from_reason("review attestation scope mismatch"));
        }
        self.learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.repository().await?;
                repository
                    .authorize_imported_review(
                        &actor,
                        &request.archive_digest,
                        &request.evidence,
                        request.decision_id,
                        request.expires_at,
                    )
                    .await?;
                Ok::<_, plasm_agent_core::workflow_learning::native::NativeLearningError>(())
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))
    }

    /// Validate and adopt an archive using the host's authorized destination.
    #[napi]
    pub async fn import_workflow_archive(
        &self,
        source: String,
        policy_json: String,
    ) -> Result<String> {
        use plasm_agent_core::workflow_learning::filesystem_archive::ImportPolicy;
        let policy = parse_archive_metadata::<ImportPolicy>(&policy_json)?;
        let admin = self.workflow_admin().await?;
        let generation = self.administrative_generation().await?;
        let mut validation_engine = generation
            .engine(
                self.workflow_config
                    .get()
                    .cloned()
                    .ok_or_else(|| Error::from_reason("workflow learning is not configured"))?,
                self.python_pool.clone(),
            )
            .map_err(map_err)?;
        let report = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let context = validation_engine.archive_validation_context()?;
                let repository = admin.owner.repository().await.map_err(
                    plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                )?;
                let report = repository
                    .import_filesystem_archive(
                        &admin.scope,
                        std::path::Path::new(&source),
                        &policy,
                        &context.host,
                        &context.session,
                        &context.authorization,
                    )
                    .await
                    .map_err(
                        plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                    )?;
                Ok::<_, engine::AgentEngineError>(report)
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&report).map_err(map_err)
    }

    /// Verify an archive's integrity and its adoption in the current scope.
    #[napi]
    pub async fn validate_workflow_archive(&self, source: String) -> Result<String> {
        let admin = self.workflow_admin().await?;
        let manifest = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.repository().await?;
                let manifest = repository
                    .validate_imported_filesystem_archive(
                        &admin.scope,
                        std::path::Path::new(&source),
                    )
                    .await?;
                Ok::<_, plasm_agent_core::workflow_learning::native::NativeLearningError>(manifest)
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&manifest).map_err(map_err)
    }

    /// Host-only bounded serving verification; never runs the checked programs.
    #[napi]
    pub async fn verify_workflow_library(&self, limit: u32) -> Result<String> {
        use workflow_verification::{LibraryVerification, MAX_VERIFIED_EXAMPLES};
        if limit == 0 || limit > MAX_VERIFIED_EXAMPLES {
            return Err(Error::from_reason("verification limit must be 1..32"));
        }
        let admin = self.workflow_admin().await?;
        let generation = self.administrative_generation().await?;
        let library = self
            .workflow_config
            .get()
            .and_then(NativeLearningConfig::library)
            .cloned()
            .ok_or_else(|| Error::from_reason("workflow library is not configured"))?;
        let pool = self.python_pool.clone();
        let report = self
            .learning_pool
            .run_local(move || async move {
                let _guard = admin.guard;
                let repository = admin.owner.repository().await.map_err(
                    plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                )?;
                let mut eligible = repository
                    .list_eligible(&admin.scope, limit + 1)
                    .await
                    .map_err(
                        plasm_agent_core::workflow_learning::native::NativeLearningError::from,
                    )?;
                let truncated = eligible.len() > limit as usize;
                eligible.truncate(limit as usize);
                let mut examples = Vec::with_capacity(eligible.len());
                for revision in eligible {
                    let mut engine = generation.engine(
                        NativeLearningConfig::Examples(library.clone()),
                        pool.clone(),
                    )?;
                    examples.push(engine.verify_workflow_example(&revision).await?);
                }
                Ok::<_, engine::AgentEngineError>(LibraryVerification {
                    scope: admin.scope,
                    generation: generation.pin.generation,
                    limit,
                    truncated,
                    examples,
                })
            })
            .await
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        serde_json::to_string(&report).map_err(map_err)
    }

    /// Close admission, drain native callbacks and terminate pooled workers.
    /// Cancellation leaves admission closed; a subsequent close completes cleanup.
    #[napi]
    pub async fn close(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
        self.cancel_effect_continuations(None);
        lock(&self.effect_pauses).clear();
        let mut drained = self.drained.write().await;
        if !*drained {
            self.python_pool.close().await;
            *drained = true;
        }
    }

    #[napi]
    pub async fn load_catalog(&self, catalog_dir: String) -> Result<JsCatalogInfo> {
        let _admission = self.admit().await?;
        let mut engine = self.inner.lock().await;
        let info = engine
            .load_catalog(PathBuf::from(catalog_dir).as_path())
            .map_err(map_err)?;
        Ok(JsCatalogInfo {
            entry_id: info.entry_id,
            catalog_cgs_hash: info.catalog_cgs_hash,
        })
    }

    /// Activate the complete loaded manifest set using explicit deployment bindings.
    #[napi]
    pub async fn activate_discovery(
        &self,
        deployment_id: String,
        bindings_json: String,
    ) -> Result<String> {
        let _admission = self.admit().await?;
        let (paths, allowed) = {
            let engine = self.inner.lock().await;
            (engine.packed_manifests(), engine.allowed_catalogs())
        };
        let bindings: plasm_core::prerequisites::DeploymentBindings =
            serde_json::from_str(&bindings_json)
                .map_err(NodeEngineError::from)
                .map_err(map_err)?;
        let catalogs = paths
            .iter()
            .map(|p| PreparedCatalog::load(p))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(NodeEngineError::from)
            .map_err(map_err)?;
        let store = self
            .discovery_store
            .get_or_try_init(|| async {
                let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
                    .or_else(|_| std::env::var("DATABASE_URL"))
                    .map_err(|_| NodeEngineError::DiscoveryDatabaseUrlMissing)?;
                let store = DiscoveryStore::connect(&url).await?;
                store.migrate().await?;
                Ok::<_, NodeEngineError>(store)
            })
            .await
            .map_err(map_err)?;
        let generation = store
            .import(&deployment_id, catalogs, &bindings)
            .await
            .map_err(NodeEngineError::from)
            .map_err(map_err)?;
        *self.activated.write().await = Some((generation.clone(), allowed));
        Ok(generation)
    }

    /// Typed intent provenance with its current discovery need; selection runs
    /// outside the execution mutex.
    #[napi]
    pub async fn route_intent(
        &self,
        intent_provenance_json: String,
        logical_session_id: Option<String>,
        session_clock: Option<String>,
    ) -> Result<String> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let learning_serves = self.workflow_config.get().is_some_and(|config| {
            matches!(
                config.mode(),
                NativeLearningMode::Examples | NativeLearningMode::Admission
            ) || (matches!(config.mode(), NativeLearningMode::Collect) && config.serves_examples())
        });
        let _discovery_admission = if learning_serves {
            self.admit_discovery(logical_session_id.as_deref()).await?
        } else {
            None
        };
        if let Some(id) = logical_session_id.as_deref() {
            let gate = lock(&self.session_admission)
                .get(id)
                .cloned()
                .ok_or_else(|| map_err(NodeEngineError::UnknownSession { id: id.to_owned() }))?;
            if gate.clock != session_clock {
                return Err(Error::from_reason("session clock cannot be changed"));
            }
        }
        let session_pool = match session_clock.as_deref() {
            Some(clock) => Arc::new(self.python_pool.for_session_clock(clock).map_err(map_err)?),
            None => self.python_pool.clone(),
        };
        let provenance: plasm_agent_core::intent_provenance::IntentProvenance =
            serde_json::from_str(&intent_provenance_json)
                .map_err(NodeEngineError::from)
                .map_err(map_err)?;
        let store = self
            .discovery_store
            .get()
            .ok_or(NodeEngineError::DiscoveryNotActivated)
            .map_err(map_err)?;
        let (generation, allowed, exposed) = self
            .routing_inputs(logical_session_id.as_deref())
            .await
            .map_err(map_err)?;
        let mut recall_ticket = None;
        let guidance = if let (Some(config), Some(owner)) =
            (self.workflow_config.get(), self.workflow_owner.get())
        {
            let exposure = match config.mode() {
                NativeLearningMode::Admission => Some(plasm_agent_core::workflow_learning::native_recall::ExampleExposure::AdmissionOnly),
                NativeLearningMode::Examples => Some(plasm_agent_core::workflow_learning::native_recall::ExampleExposure::OptionalExamples),
                NativeLearningMode::Collect if config.serves_examples() => Some(plasm_agent_core::workflow_learning::native_recall::ExampleExposure::OptionalExamples),
                _ => None,
            };
            match exposure {
                Some(exposure) => {
                    use plasm_agent_core::workflow_learning::deferred_recall::{
                        RecallContext, RecallStatus,
                    };
                    use plasm_agent_core::workflow_learning::native_recall::{
                        ExampleGuidance, RecallDisposition,
                    };
                    let context = RecallContext {
                        scope: config.scope().map_err(map_err)?,
                        intent: provenance.current().to_owned(),
                        intent_revision:
                            plasm_agent_core::workflow_learning::deferred_recall::intent_revision(
                                &provenance,
                            ),
                        generation: generation.clone(),
                        authorization: allowed.clone(),
                    };
                    if let Some(session) = logical_session_id.as_deref() {
                        owner
                            .deferred_recalls
                            .discard_stale_supplemental(session, &context, &exposed);
                    }
                    let status = logical_session_id
                        .as_deref()
                        .map(|id| owner.deferred_recalls.status(id, &context))
                        .unwrap_or(RecallStatus::Missing);
                    Some(match status {
                        RecallStatus::Completed((examples, associations)) => {
                            let validation_owner = Arc::clone(owner);
                            let validation_scope = context.scope.clone();
                            let checked = tokio::time::timeout(std::time::Duration::from_millis(250), self.learning_pool.run_local(move || async move {
                                plasm_agent_core::workflow_learning::deferred_recall::validate_current(&validation_owner, &validation_scope, (examples, associations)).await
                            })).await;
                            match checked {
                                Ok(Ok(Some((examples, associations)))) => {
                                    ExampleGuidance::from_authorized(
                                        examples,
                                        associations,
                                        exposure,
                                    )
                                }
                                _ => ExampleGuidance::empty(RecallDisposition::Unavailable),
                            }
                        }
                        RecallStatus::Pending => ExampleGuidance::empty(RecallDisposition::Pending),
                        RecallStatus::Unavailable => {
                            ExampleGuidance::empty(RecallDisposition::Unavailable)
                        }
                        RecallStatus::Missing => {
                            let prior = logical_session_id
                                .as_deref()
                                .map(|session| {
                                    owner
                                        .deferred_recalls
                                        .prior_associations_for_session(session, &context)
                                })
                                .unwrap_or_default();
                            recall_ticket = owner.deferred_recalls.start(
                                Arc::clone(owner),
                                context,
                                store.clone(),
                                prior,
                                exposed.clone(),
                            );
                            ExampleGuidance::empty(if recall_ticket.is_some() {
                                RecallDisposition::Pending
                            } else {
                                RecallDisposition::Unavailable
                            })
                        }
                    })
                }
                None => None,
            }
        } else {
            None
        };
        let service = DiscoveryService::from_env(store.clone())
            .map_err(NodeEngineError::from)
            .map_err(map_err)?;
        let receipt = service
            .route_turn(RouteTurn {
                new_generation: &generation,
                intent_provenance: &provenance,
                logical_session: logical_session_id.as_deref(),
                allowed: &allowed,
                exposed: &exposed,
                expires_at: std::time::SystemTime::now()
                    + std::time::Duration::from_secs(24 * 60 * 60),
            })
            .await
            .map_err(NodeEngineError::from)
            .map_err(map_err)?;
        if let (Some(ticket), Some(owner)) = (recall_ticket, self.workflow_owner.get()) {
            owner.deferred_recalls.bind(ticket, &receipt.pin_id);
        }
        let teaching = {
            let (catalogs, compiled_catalogs, prerequisite_deployments) = store
                .load_generation(&receipt.retrieval.generation)
                .await
                .map_err(NodeEngineError::from)
                .map_err(map_err)?;
            let pin = DiscoverySessionPin {
                authorization: receipt.authorization.clone(),
                generation: receipt.retrieval.generation.clone(),
                pin_id: receipt.pin_id.clone(),
            };
            let shared = {
                let mut sessions = lock(&self.sessions);
                lock(&self.session_admission)
                    .entry(receipt.pin_id.clone())
                    .or_insert_with(|| Arc::new(SessionAdmission::new(session_clock.clone())));
                sessions
                    .entry(receipt.pin_id.clone())
                    .or_insert_with(|| {
                        let mut engine = InnerEngine::from_pinned_generation(
                            catalogs,
                            compiled_catalogs,
                            pin.clone(),
                            prerequisite_deployments,
                            session_pool,
                        );
                        engine
                            .configure_workflow_learning(
                                self.workflow_config.get().cloned().unwrap_or_default(),
                            )
                            .expect("validated configuration on a fresh engine");
                        Arc::new(AsyncMutex::new(engine))
                    })
                    .clone()
            };
            let mut engine = shared.lock().await;
            if engine.discovery_pin() != Some(&pin) {
                return Err(map_err(NodeEngineError::RoutingPinMismatch));
            }
            match receipt.closure.as_ref() {
                Some(closure) => Some(
                    engine
                        .expose_routing(&receipt.intent, closure)
                        .await
                        .map_err(map_err)?,
                ),
                None => {
                    engine.retain_unexposed_intent(&receipt.intent_provenance);
                    None
                }
            }
        };
        let mut packet = serde_json::json!({"routing":receipt,"teaching":teaching});
        if let Some(guidance) = guidance {
            packet["workflow_examples"] =
                serde_json::to_value(guidance.examples).map_err(map_err)?;
            packet["workflow_recall"] =
                serde_json::to_value(guidance.disposition).map_err(map_err)?;
        }
        serde_json::to_string(&packet).map_err(map_err)
    }

    /// Deliver completed, still-authorized examples on a later session turn.
    #[napi]
    pub async fn take_completed_teaching(
        &self,
        logical_session_id: String,
        include_examples: Option<bool>,
    ) -> Result<String> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(Some(&logical_session_id)).await?;
        let _discovery_admission = self.admit_discovery(Some(&logical_session_id)).await?;
        let (Some(config), Some(owner)) = (self.workflow_config.get(), self.workflow_owner.get())
        else {
            return Ok(String::new());
        };
        if !matches!(
            config.mode(),
            NativeLearningMode::Examples | NativeLearningMode::Admission
        ) && !(matches!(config.mode(), NativeLearningMode::Collect) && config.serves_examples())
        {
            return Ok(String::new());
        }
        let include_examples = include_examples.unwrap_or(true)
            && !matches!(config.mode(), NativeLearningMode::Admission);
        if !owner
            .deferred_recalls
            .has_delivery(&logical_session_id, include_examples)
        {
            return Ok(String::new());
        }
        let scope = config.scope().map_err(map_err)?;
        let (generation, allowed, _) = self
            .routing_inputs(Some(&logical_session_id))
            .await
            .map_err(map_err)?;
        let mut cards = Vec::new();
        if let Some(store) = self.discovery_store.get() {
            let provenance = store.intent_provenance(&logical_session_id).await;
            if let Err(error) = &provenance {
                tracing::warn!(target: "plasm_agent::workflow_learning", %error, "could not load the session intent for supplemental delivery");
            }
            if let Ok(provenance) = provenance {
                let context = plasm_agent_core::workflow_learning::deferred_recall::RecallContext {
                    scope: scope.clone(),
                    intent: provenance.current().to_owned(),
                    intent_revision:
                        plasm_agent_core::workflow_learning::deferred_recall::intent_revision(
                            &provenance,
                        ),
                    generation: generation.clone(),
                    authorization: allowed.clone(),
                };
                if owner
                    .deferred_recalls
                    .take_supplemental_failure(&logical_session_id, &context)
                {
                    cards.push("Supplemental discovery is unavailable; ordinary discovery remains available.".into());
                }
                if let Some(claim) = owner
                    .deferred_recalls
                    .claim_supplemental(&logical_session_id, &context)
                {
                    let expected = claim.result().associations.len();
                    let associations = claim.result().associations.clone();
                    let validation_owner = Arc::clone(owner);
                    let validation_scope = scope.clone();
                    let checked = self
                        .learning_pool
                        .run_local(move || async move {
                            plasm_agent_core::workflow_learning::deferred_recall::validate_current(
                                &validation_owner,
                                &validation_scope,
                                (Vec::new(), associations),
                            )
                            .await
                        })
                        .await;
                    if let Ok(Some((_, current))) = checked {
                        if current.len() == expected {
                            let shared =
                                self.session_engine(&logical_session_id).map_err(map_err)?;
                            let mut engine = shared.lock().await;
                            let pin_current = engine.discovery_pin().is_some_and(|pin| {
                                pin.generation == generation && pin.authorization == allowed
                            });
                            let intent_current = store
                                .intent_provenance(&logical_session_id)
                                .await
                                .is_ok_and(|current| {
                                    plasm_agent_core::workflow_learning::deferred_recall::intent_revision(&current) == context.intent_revision
                                });
                            if pin_current && intent_current {
                                let wave = engine
                                    .expose_routing(&context.intent, &claim.result().closure)
                                    .await
                                    .map_err(map_err)?;
                                tracing::debug!(target: "plasm_agent::workflow_learning", declarations_changed = wave.declarations_changed, prompt_chars = wave.prompt.len(), "supplemental discovery delivered to embedded session");
                                if wave.declarations_changed {
                                    cards.push(format!(
                                        "Additional discovery for the current request:\n{}",
                                        wave.prompt
                                    ));
                                }
                                claim.commit();
                            }
                        } else {
                            claim.commit();
                        }
                    }
                }
            }
        }
        let completed = if include_examples {
            owner.deferred_recalls.completed_for_session(
                &logical_session_id,
                &scope,
                &generation,
                &allowed,
            )
        } else {
            Vec::new()
        };
        for item in completed
            .into_iter()
            .filter(|item| !item.examples.is_empty())
            .take(3)
        {
            let validation_owner = Arc::clone(owner);
            let validation_scope = scope.clone();
            let checked = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                self.learning_pool.run_local(move || async move {
                    plasm_agent_core::workflow_learning::deferred_recall::validate_current(
                        &validation_owner,
                        &validation_scope,
                        (item.examples, item.associations),
                    )
                    .await
                }),
            )
            .await;
            if let Ok(Ok(Some((examples, associations)))) = checked {
                let exposure = if matches!(config.mode(), NativeLearningMode::Admission) {
                    plasm_agent_core::workflow_learning::native_recall::ExampleExposure::AdmissionOnly
                } else {
                    plasm_agent_core::workflow_learning::native_recall::ExampleExposure::OptionalExamples
                };
                let guidance = plasm_agent_core::workflow_learning::native_recall::ExampleGuidance::from_authorized(examples, associations, exposure);
                let fresh = owner.deferred_recalls.claim_delivery(
                    item.ticket,
                    &guidance
                        .examples
                        .iter()
                        .map(|example| example.example_ref)
                        .collect::<Vec<_>>(),
                );
                if !fresh.is_empty() {
                    cards.push(format!(
                        "Optional workflow examples for {} (fetch with plasm_example):\n{}",
                        item.intent,
                        guidance
                            .examples
                            .iter()
                            .filter(|example| fresh.contains(&example.example_ref))
                            .map(|example| format!(
                                "- {}: {}",
                                example.example_ref, example.purpose
                            ))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
            }
        }
        Ok(cards.join("\n\n"))
    }

    #[napi]
    pub async fn expose_seeds(
        &self,
        intent: String,
        seeds: Vec<JsSeed>,
    ) -> Result<JsTeachingResult> {
        let _admission = self.admit().await?;
        let mut engine = self.inner.lock().await;
        let capability_seeds: Vec<CapabilitySeed> = seeds
            .into_iter()
            .map(|s| CapabilitySeed {
                entry_id: s.api,
                entity: s.entity,
            })
            .collect();
        let result = engine
            .expose_seeds(intent, &capability_seeds)
            .await
            .map_err(map_err)?;
        Ok(JsTeachingResult {
            prompt: result.prompt,
            declarations_changed: result.declarations_changed,
            delta_refs: result.delta_refs,
        })
    }

    #[napi]
    pub async fn introspect_catalog(&self, entry_id: String) -> Result<String> {
        let _admission = self.admit().await?;
        let engine = self.inner.lock().await;
        let info = engine.introspect_catalog(&entry_id).map_err(map_err)?;
        serde_json::to_string(&info).map_err(|e| Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn dry_run(
        &self,
        program: String,
        logical_session_id: Option<String>,
    ) -> Result<JsDryRunResult> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let result = engine.dry_run(&program).await.map_err(map_err)?;
        Ok(JsDryRunResult {
            write_count: result.write_count,
            failure_json: result.failure_json,
            plan_commit_ref: result.plan_commit_ref,
            summary: result.summary,
            comp_json: serde_json::to_string(&result.comp_json)
                .map_err(|e| Error::from_reason(e.to_string()))?,
            fused_clean_read: result.fused_clean_read,
        })
    }

    #[napi]
    pub async fn run_plan(
        &self,
        plan_commit_ref: String,
        logical_session_id: Option<String>,
    ) -> Result<JsRunPlanResult> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let result = engine.run_plan(&plan_commit_ref).map_err(map_err)?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
            artifacts_json: result.artifacts_json,
            failure_json: result.failure_json,
            continuation_ref: None,
        })
    }

    #[napi(
        ts_args_type = "planCommitRef: string, transport: (request: JsTransportRequest) => JsTransportResponse | Promise<JsTransportResponse>, logicalSessionId?: string"
    )]
    pub async fn run_plan_live(
        &self,
        plan_commit_ref: String,
        transport: JsHostTransport,
        logical_session_id: Option<String>,
    ) -> Result<JsRunPlanResult> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let callback = JsCallbackHttpTransport::new(transport.0, engine.primary_entry_id());
        let workflow = logical_session_id.unwrap_or_default();
        let (control, mut pauses) =
            plasm_runtime::execution::effect_continuation::EffectContinuation::new_native(
                plasm_runtime::execution::effect_continuation::MAX_EFFECT_CONTINUATION_LEASE,
            )
            .map_err(map_err)?;
        let mut reservation = self
            .effect_obligations
            .admit(&workflow, control.clone())
            .map_err(Error::from_reason)?;
        let reference = control.native_reference();
        lock(&self.effect_controls).insert(reference.clone(), (workflow.clone(), control.clone()));
        if self.closing.load(std::sync::atomic::Ordering::Acquire)
            || (!workflow.is_empty()
                && lock(&self.session_admission)
                    .get(&workflow)
                    .is_some_and(|gate| gate.sealed.load(std::sync::atomic::Ordering::Acquire)))
        {
            control.cancel();
        }
        let active = ActiveEffectGuard {
            reference: reference.clone(),
            controls: self.effect_controls.clone(),
        };
        let worker_control = control.clone();
        let mut worker = self
            .learning_pool
            .start_local(move || async move {
                let _admission = _admission;
                let _session_admission = _session_admission;
                let _active = active;
                let prepared = match engine.prepare_live_run(&plan_commit_ref, callback).await {
                    Ok(value) => value,
                    Err(error) => {
                        reservation.preparation_failed();
                        return Err(error);
                    }
                };
                drop(engine);
                let result = match prepared {
                    engine::PreparedLiveRunResult::Rejected(result) => Ok(result),
                    engine::PreparedLiveRunResult::Ready(prepared) => {
                        reservation.begin_execution();
                        let (outcome, certificate) = worker_control
                            .clone()
                            .scope_for_repair(async move {
                                match prepared.execute().await {
                                    Ok(result) if result.ok => Ok(result),
                                    other => Err(other),
                                }
                            })
                            .await
                            .map_err(engine::AgentEngineError::EffectContinuation)?;
                        match outcome {
                            Ok(mut result) => {
                                let resumed = worker_control.has_checkpoint();
                                let discharged = reservation.finish(&result, certificate);
                                if resumed && !discharged {
                                    result.message.push_str("\n\nThe original run returned, but its native repair evidence was incomplete, expired, or invalid. No repair certificate was issued; the obligation remains open. Do not repeat effects without reconciliation.");
                                }
                                return Ok(result);
                            }
                            Err(result) => result,
                        }
                    }
                };
                if let Ok(value) = &result {
                    reservation.finish(value, None);
                }
                result
            })
            .await
            .map_err(map_err)?;
        let result = tokio::select! {
            biased;
            result = worker.wait() => result.map_err(map_err)?.map_err(|error| self.workflow_error(error))?,
            pause = pauses.recv() => {
                let pause = pause.ok_or_else(|| Error::from_reason("effect checkpoint channel closed"))?;
                let failure_json = serde_json::to_string(&pause.failure).map_err(map_err)?;
                let continuation_ref = pause.native_reference();
                let deadline = control.checkpoint_deadline().ok_or_else(|| Error::from_reason("effect checkpoint has no deadline"))?;
                let entry = PendingEffectEntry {
                    workflow: workflow.clone(), deadline, control: control.clone(),
                    pending: Arc::new(AsyncMutex::new(PendingEffect { workflow, reference: continuation_ref.clone(), control, pause: Some(pause), pauses, worker })),
                };
                let mut retained = lock(&self.effect_pauses);
                prune_expired_effects(&mut retained);
                if self.closing.load(std::sync::atomic::Ordering::Acquire)
                    || (!entry.workflow.is_empty() && lock(&self.session_admission).get(&entry.workflow)
                        .is_none_or(|gate| gate.sealed.load(std::sync::atomic::Ordering::Acquire)))
                {
                    entry.control.cancel();
                    return Err(Error::from_reason("workflow closed while effect was suspended"));
                }
                if retained.len() >= 128 {
                    entry.control.cancel();
                    return Err(Error::from_reason("effect continuation capacity exhausted; the original run remains unresolved"));
                }
                if retained.contains_key(&continuation_ref) {
                    entry.control.cancel();
                    return Err(Error::from_reason("effect continuation identity collision"));
                }
                retained.insert(continuation_ref.clone(), entry);
                drop(retained);
                return Ok(JsRunPlanResult {
                    ok: false,
                    message: "Execution paused after a confirmed no-effect rejection. Earlier effects may have completed. Resume only this original execution.".into(),
                    rows_json: None, meta_json: None, artifacts_json: None,
                    failure_json: Some(failure_json), continuation_ref: Some(continuation_ref),
                });
            }
        };
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
            artifacts_json: result.artifacts_json,
            failure_json: result.failure_json,
            continuation_ref: None,
        })
    }

    /// Atomically accept a terminal across the caller's native workflows.
    #[napi]
    pub async fn seal_effect_workflows(&self, workflows_json: String) -> Result<()> {
        let _admission = self.admit().await?;
        let ids: Vec<String> = parse_archive_metadata(&workflows_json)?;
        self.effect_obligations
            .seal(&ids)
            .map_err(Error::from_reason)
    }

    /// Resume only the retained native execution belonging to this workflow.
    #[napi]
    pub async fn resume_effect_run(
        &self,
        continuation_ref: String,
        logical_session_id: Option<String>,
    ) -> Result<JsRunPlanResult> {
        let _admission = self.admit().await?;
        let _session_admission = self.admit_session(logical_session_id.as_deref()).await?;
        let entry = {
            let mut retained = lock(&self.effect_pauses);
            prune_expired_effects(&mut retained);
            retained
                .get(&continuation_ref)
                .cloned()
                .ok_or_else(|| Error::from_reason("effect continuation is unknown or expired"))?
        };
        if entry.workflow != logical_session_id.as_deref().unwrap_or("") {
            return Err(Error::from_reason(
                "effect continuation belongs to another workflow",
            ));
        }
        let mut pending = entry.pending.lock().await;
        if pending.workflow != logical_session_id.as_deref().unwrap_or("") {
            return Err(Error::from_reason(
                "effect continuation belongs to another workflow",
            ));
        }
        if pending.reference != continuation_ref {
            return Err(Error::from_reason(
                "effect continuation has already been consumed",
            ));
        }
        if let Some(mut pause) = pending.pause.take() {
            match pending.control.resume(&mut pause) {
                Ok(()) | Err(plasm_runtime::execution::effect_continuation::EffectContinuationError::AlreadyResolved) => {}
                Err(error) => return Err(map_err(error)),
            }
        }
        let PendingEffect { worker, pauses, .. } = &mut *pending;
        let result = tokio::select! {
            biased;
            result = worker.wait() => result,
            pause = pauses.recv() => {
                let pause = pause.ok_or_else(|| Error::from_reason("effect checkpoint channel closed"))?;
                let failure_json = serde_json::to_string(&pause.failure).map_err(map_err)?;
                let next_ref = pause.native_reference();
                pending.pause = Some(pause);
                pending.reference = next_ref.clone();
                drop(pending);
                let mut retained = lock(&self.effect_pauses);
                retained.remove(&continuation_ref);
                if retained.contains_key(&next_ref) {
                    entry.control.cancel();
                    return Err(Error::from_reason("effect continuation identity collision"));
                }
                retained.insert(next_ref.clone(), entry.clone());
                return Ok(JsRunPlanResult { ok: false,
                    message: "Execution paused again after a confirmed no-effect rejection. Resume only this original execution.".into(),
                    rows_json: None, meta_json: None, artifacts_json: None,
                    failure_json: Some(failure_json), continuation_ref: Some(next_ref) });
            }
        };
        pending.reference.clear();
        drop(pending);
        lock(&self.effect_pauses).remove(&continuation_ref);
        let result = result
            .map_err(map_err)?
            .map_err(|error| self.workflow_error(error))?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
            artifacts_json: result.artifacts_json,
            failure_json: result.failure_json,
            continuation_ref: None,
        })
    }
}

fn parse_archive_metadata<T: serde::de::DeserializeOwned>(json: &str) -> Result<T> {
    if json.len() > 1024 * 1024 {
        return Err(Error::from_reason(
            "archive metadata exceeds bounded admission limits",
        ));
    }
    serde_json::from_str(json).map_err(map_err)
}

#[cfg(test)]
mod mutex_law_tests {
    use super::*;

    #[test]
    fn node_error_retains_catalog_failure_cause() {
        let error = engine::AgentEngineError::from(plasm_core::catalog_il::CatalogIlError::Io(
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog fixture denied",
            ),
        ));
        let rendered = map_err(error).reason;
        assert!(rendered.contains("catalog interchange failed"));
        assert!(rendered.contains("catalog fixture denied"));
    }

    #[tokio::test]
    async fn extension_keeps_generation_and_policy_after_activation_changes() {
        let engine = PlasmEngine::new();
        let authorization = DiscoveryAuthorization {
            catalogs: ["retained".into()].into(),
            capabilities: [("retained".into(), ["read".into()].into())].into(),
        };
        let pin = DiscoverySessionPin {
            generation: "retained-generation".into(),
            authorization: authorization.clone(),
            pin_id: "logical-session".into(),
        };
        lock(&engine.sessions).insert(
            pin.pin_id.clone(),
            Arc::new(AsyncMutex::new(InnerEngine::from_pinned_generation(
                Default::default(),
                Default::default(),
                pin,
                Default::default(),
                engine.python_pool.clone(),
            ))),
        );
        *engine.activated.write().await =
            Some(("new-generation".into(), ["replacement".into()].into()));
        let (generation, policy, _) = engine
            .routing_inputs(Some("logical-session"))
            .await
            .unwrap();
        assert_eq!(generation, "retained-generation");
        assert_eq!(policy, authorization);
        let (generation, policy, _) = engine.routing_inputs(None).await.unwrap();
        assert_eq!(generation, "new-generation");
        assert_eq!(policy.catalogs, ["replacement".into()].into());
        assert!(engine.routing_inputs(Some("missing")).await.is_err());
        assert!(
            engine
                .refresh_discovery_session(
                    &*engine
                        .session_engine("logical-session")
                        .unwrap()
                        .lock()
                        .await
                )
                .await
                .is_err(),
            "routed execution must fail when its discovery store is unavailable"
        );
    }

    /// Full cutover guard: sync acquisition on the shared Tokio mutex must not return.
    #[tokio::test]
    async fn close_drains_admitted_calls_and_rejects_new_calls() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;
        let engine = PlasmEngine::new();
        let mut held = engine.inner.lock().await;
        *held = InnerEngine::from_generation(
            [("fixture".into(), plasm_core::CGS::new())].into(),
            Default::default(),
            "closing-session".into(),
            engine.python_pool.clone(),
        );
        let mut admitted = Box::pin(engine.introspect_catalog("fixture".into()));
        poll_fn(|cx| {
            assert!(admitted.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let mut closing = Box::pin(engine.close());
        poll_fn(|cx| {
            assert!(closing.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(engine.introspect_catalog("fixture".into()).await.is_err());
        drop(held);
        assert!(admitted.await.is_ok());
        closing.await;
        assert!(*engine.drained.read().await);
        assert!(engine.introspect_catalog("fixture".into()).await.is_err());
        engine.close().await;
    }

    #[tokio::test]
    async fn draining_one_session_waits_for_its_work_without_stopping_another() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;

        let engine = PlasmEngine::new();
        for id in ["a", "b"] {
            lock(&engine.session_admission)
                .insert(id.into(), Arc::new(SessionAdmission::new(None)));
        }
        let admitted = engine.admit_session(Some("a")).await.unwrap();
        let mut drain = Box::pin(engine.drain_session("a".into()));
        poll_fn(|cx| {
            assert!(drain.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(engine.admit_session(Some("b")).await.is_ok());
        drop(admitted);
        drain.await.unwrap();
        assert!(engine.admit_session(Some("a")).await.is_err());
        assert!(engine.admit_session(Some("b")).await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_session_drain_still_prevents_new_work() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;

        let engine = PlasmEngine::new();
        lock(&engine.session_admission).insert("a".into(), Arc::new(SessionAdmission::new(None)));
        let admitted = engine.admit_session(Some("a")).await.unwrap();
        let mut drain = Box::pin(engine.drain_session("a".into()));
        poll_fn(|cx| {
            assert!(drain.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(drain);
        assert!(engine.admit_session(Some("a")).await.is_err());
        drop(admitted);
        engine.drain_session("a".into()).await.unwrap();
    }

    #[tokio::test]
    async fn releasing_one_session_preserves_other_sessions() {
        let engine = PlasmEngine::new();
        for id in ["a", "b"] {
            lock(&engine.sessions).insert(
                id.into(),
                Arc::new(AsyncMutex::new(InnerEngine::new(
                    engine.python_pool.clone(),
                ))),
            );
            lock(&engine.session_admission)
                .insert(id.into(), Arc::new(SessionAdmission::new(None)));
        }
        engine.release_session("a".into()).await.unwrap();
        assert!(engine.session_engine("a").is_err());
        assert!(engine.session_engine("b").is_ok());
        assert!(engine.admit_session(Some("b")).await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_close_keeps_admission_closed_and_can_finish() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;
        let engine = PlasmEngine::new();
        let admitted = engine.admit().await.unwrap();
        let mut closing = Box::pin(engine.close());
        poll_fn(|cx| {
            assert!(closing.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(closing);
        assert!(engine.admit().await.is_err());
        drop(admitted);
        engine.close().await;
        assert!(*engine.drained.read().await);
    }

    #[test]
    fn plasm_engine_napi_surface_forbids_blocking_lock() {
        let src = include_str!("lib.rs");
        let forbidden = concat!("self.inner.", "blocking_lock", "(");
        assert!(
            !src.contains(forbidden),
            "plasm-node NAPI surface must use lock().await only — sync mutex acquisition deadlocks under parallel JS tool calls"
        );
    }

    #[tokio::test]
    async fn calls_queue_and_cancel_without_losing_the_engine() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;
        let engine = PlasmEngine::new();
        let mut held = engine.inner.lock().await;
        *held = InnerEngine::from_generation(
            [("fixture".into(), plasm_core::CGS::new())].into(),
            Default::default(),
            "queued-session".into(),
            engine.python_pool.clone(),
        );
        let mut first = Box::pin(engine.dry_run("invalid".into(), None));
        let mut cancelled = Box::pin(engine.introspect_catalog("fixture".into()));
        let mut second = Box::pin(engine.introspect_catalog("fixture".into()));
        poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            assert!(cancelled.as_mut().poll(cx).is_pending());
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        // Cancelling a queued caller must not obstruct those behind it.
        drop(cancelled);
        drop(held);
        assert!(
            first.await.is_err(),
            "invalid program should fail after admission"
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .unwrap();
        assert!(
            result.is_ok(),
            "queued read must succeed after the preceding call fails: {result:?}"
        );
        assert!(
            engine.inner.try_lock().is_ok(),
            "failed call releases admission"
        );
    }

    #[tokio::test]
    async fn logical_sessions_have_independent_queues_and_survive_holder_cancellation() {
        let engine = Arc::new(PlasmEngine::new());
        for id in ["a", "b"] {
            lock(&engine.sessions).insert(
                id.into(),
                Arc::new(AsyncMutex::new(InnerEngine::new(
                    engine.python_pool.clone(),
                ))),
            );
        }
        let a = engine.session_engine("a").unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            let _held = a.lock().await;
            tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        rx.await.unwrap();
        let b = engine.session_engine("b").unwrap();
        assert!(
            b.try_lock().is_ok(),
            "different sessions must not share admission"
        );
        holder.abort();
        assert!(holder.await.unwrap_err().is_cancelled());
        let a = engine.session_engine("a").unwrap();
        assert!(
            a.try_lock().is_ok(),
            "cancellation must release the retained engine"
        );
    }
}
