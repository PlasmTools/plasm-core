//! In-process agent engine: catalog load, agent-global symbol exposure, dry-run.

use indexmap::IndexMap;
use plasm_agent_core::discovery_store::DiscoverySessionPin;
use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state_with_learning, PlasmHostBootstrap};
use plasm_agent_core::http_execute::CapabilitySeed;
use plasm_agent_core::operation::{
    compute_plan_commit_id_from_dry, PlanCommitRecord, PLAN_COMMIT_TTL,
};
use plasm_agent_core::plan_commit_store::{dry_for_committed_plasm_run, resolve_committed_plan};
use plasm_agent_core::plasm_compile::compile_program;
use plasm_agent_core::plasm_plan_run::run_plasm_comp_python as run_plasm_comp;
use plasm_agent_core::plasm_plan_run::{
    evaluate_plasm_comp_dry, plan_dry_compact_view, render_plasm_plan_dry_text_for_session,
};
use plasm_agent_core::program_diagnostic::{ProgramDiagnostic, ProgramStageError};
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_agent_core::workflow_learning::native::{
    NativeLearningConfig, NativeLearningError, NativeLearningMode,
};
use plasm_agent_core::PlasmCompBundle;
use plasm_core::discovery::{CgsRegistry, RegistryEntryPair};
use plasm_core::run_reference::ExecutableRunTarget;
use plasm_core::PlanCommitRef;
use plasm_core::{
    capability_method_label_kebab, ExposureEntityKey, NamedValueSchema, OutputSchema,
    PromptPipelineConfig, SymbolMapCrossRequestCache, TeachingExposureSession, CGS,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

/// Stable execute-session wire ids for in-process agent runs (run artifact store keys).
const AGENT_PROMPT_HASH: &str = "e45f6d1eb01b0ae077a369e44b9f1571169db0f3c8edfe9e377a9fab30c92696";

type Result<T> = std::result::Result<T, AgentEngineError>;

#[derive(Debug, Error)]
pub enum AgentEngineError {
    #[error("native execution engine construction failed")]
    Runtime(#[source] Box<plasm_runtime::RuntimeError>),
    #[error("native workflow learning: {0}")]
    Workflow(#[source] Box<NativeLearningError>),
    #[error("workflow configuration must precede execution")]
    WorkflowAlreadyStarted,
    #[error("no authoritative workflow execution evidence exists")]
    WorkflowEvidenceMissing,
    #[error(transparent)]
    CatalogTemplate(#[from] plasm_compile::CatalogTemplateError),
    #[error("catalog `{entry_id}` is not loaded")]
    CatalogNotLoaded { entry_id: String },
    #[error("catalog manifest path has no containing directory")]
    ManifestDirectoryMissing,
    #[error("catalog loading requires a format-3 packed manifest")]
    PackedManifestRequired,
    #[error("catalog digest changed for `{entry_id}`; a new symbol space is required")]
    CatalogDigestChanged { entry_id: String },
    #[error("catalog `{entry_id}` is missing from the pinned generation")]
    PinnedCatalogMissing { entry_id: String },
    #[error("routing completed without a teaching exposure")]
    RoutingExposureMissing,
    #[error("catalog `{entry_id}` is not loaded")]
    SeedCatalogMissing { entry_id: String },
    #[error("entity `{entity}` is absent from catalog `{entry_id}`")]
    SeedEntityMissing { entry_id: String, entity: String },
    #[error("flow policy denied plan commit ({verdict:?}) with {violations} violation(s)")]
    PlanCommitDenied { verdict: String, violations: usize },
    #[error("plan commit reference is unknown or expired")]
    PlanCommitUnavailable(#[source] plasm_agent_core::plan_commit_store::PlanCommitVerifyError),
    #[error("run reference is unknown or expired")]
    RunReferenceUnavailable(#[source] plasm_agent_core::plan_commit_store::PlanCommitVerifyError),
    #[error("committed plan artifact is invalid: {0}")]
    CommittedPlanArtifact(#[source] plasm_agent_core::error::PlasmCompBundleError),
    #[error("dry evaluation of paging plan failed: {0}")]
    PagingPlanDry(#[source] Box<ProgramStageError>),
    #[error("canonical run artifact identifier is invalid")]
    InvalidRunArtifactId,
    #[error("canonical run artifact is missing")]
    RunArtifactMissing,
    #[error("no catalogs are loaded")]
    NoCatalogsLoaded,
    #[error("teaching exposure is unavailable")]
    TeachingExposureMissing,
    #[error("primary catalog `{entry_id}` is missing")]
    PrimaryCatalogMissing { entry_id: String },
    #[error("NAPI host state construction failed")]
    HostBootstrap(#[from] plasm_agent_core::http::HostBootstrapError),
    #[error("run artifact lookup failed")]
    RunArtifact(#[source] plasm_agent_core::run_artifacts::RunArtifactError),
    #[error("serialization failed")]
    Json(#[from] serde_json::Error),
    #[error("result value encoding failed: {0}")]
    ValueCodec(#[from] plasm_core::ValueCodecError),
    #[error("catalog interchange failed")]
    CatalogIl(#[from] Box<plasm_core::catalog_il::CatalogIlError>),
    #[error("catalog compilation failed")]
    Compile(#[from] plasm_compile::CmlError),
    #[error("teaching surface construction failed")]
    CapabilityExposure(#[from] plasm_core::capability_exposure::CapabilityExposureError),
    #[error("Python teaching preparation failed")]
    PythonTeaching(#[from] plasm_core::prompt_render::python::PythonTeachingError),
    #[error("prerequisite binding rendering failed")]
    PrerequisiteBinding(#[from] plasm_core::prompt_render::PrerequisiteBindingRenderError),
    #[error(
        "plan commit flow policy rejected the plan ({verdict}) with {violations} violation(s)"
    )]
    FlowDenial { verdict: String, violations: usize },
}

impl From<plasm_runtime::RuntimeError> for AgentEngineError {
    fn from(error: plasm_runtime::RuntimeError) -> Self {
        Self::Runtime(Box::new(error))
    }
}

impl From<NativeLearningError> for AgentEngineError {
    fn from(error: NativeLearningError) -> Self {
        Self::Workflow(Box::new(error))
    }
}

impl From<plasm_core::catalog_il::CatalogIlError> for AgentEngineError {
    fn from(source: plasm_core::catalog_il::CatalogIlError) -> Self {
        Self::CatalogIl(Box::new(source))
    }
}

impl From<ProgramStageError> for AgentEngineError {
    fn from(source: ProgramStageError) -> Self {
        Self::PagingPlanDry(Box::new(source))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EntityFieldIntrospection {
    pub name: String,
    pub value_ref: String,
    pub required: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EntityIntrospection {
    pub name: String,
    pub id_field: String,
    pub fields: Vec<EntityFieldIntrospection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PythonCapabilityBinding {
    pub entity_symbol: String,
    pub method: String,
    pub receiver: bool,
    pub identity: Vec<String>,
    pub unavailable: Option<String>,
    pub parameters: Vec<plasm_core::InputFieldSchema>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CapabilityIntrospection {
    pub name: String,
    pub kind: String,
    pub entity: String,
    pub invoke_wire_name: String,
    pub python: PythonCapabilityBinding,
    pub inputs: plasm_core::CapabilityInputs,
    pub provides: Vec<String>,
    pub output_schema: Option<OutputSchema>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogIntrospection {
    pub entry_id: String,
    pub catalog_cgs_hash: String,
    pub entities: Vec<EntityIntrospection>,
    pub values: IndexMap<String, NamedValueSchema>,
    pub capabilities: Vec<CapabilityIntrospection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogInfo {
    pub entry_id: String,
    pub catalog_cgs_hash: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeachingExposureResult {
    pub prompt: String,
    pub declarations_changed: bool,
    pub delta_refs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DryRunResult {
    /// Compiler-owned count; never inferred from rendered review text.
    pub write_count: u32,
    pub failure_json: Option<String>,
    pub plan_commit_ref: String,
    pub summary: String,
    pub comp_json: serde_json::Value,
    /// Same predicate as MCP `plasm` fused execute (clean 0-write plans).
    pub fused_clean_read: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunPlanResult {
    pub ok: bool,
    pub message: String,
    pub rows_json: Option<String>,
    pub meta_json: Option<String>,
    pub artifacts_json: Option<String>,
    pub failure_json: Option<String>,
}

impl RunPlanResult {
    /// Admission fails before this invocation can dispatch any operation.
    fn reference_rejection(code: &str, diagnostic: impl Into<String>) -> Self {
        let failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Program,
            code,
            diagnostic,
        );
        Self::failure(failure)
    }

    fn failure(failure: plasm_runtime::ExecutionFailure) -> Self {
        Self {
            ok: false,
            message: plasm_agent_core::agent_presentation::render_failure(&failure).into_string(),
            rows_json: None,
            meta_json: None,
            artifacts_json: None,
            failure_json: Some(serde_json::to_string(&failure).expect("typed failure serializes")),
        }
    }
}

fn live_run_rows_json(
    live: &plasm_agent_core::plasm_plan_run::PlasmPlanRunResult,
) -> Result<Option<String>> {
    use plasm_agent_core::output::http_execute_results_value;
    let steps = &live.return_steps;
    if steps.is_empty() {
        return Ok(None);
    }
    let value = if steps.len() == 1 {
        http_execute_results_value(&steps[0].result)?
    } else {
        serde_json::Value::Array(
            steps
                .iter()
                .map(|s| http_execute_results_value(&s.result))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )
    };
    Ok(Some(serde_json::to_string(&value)?))
}

fn live_run_meta_json(
    live: &plasm_agent_core::plasm_plan_run::PlasmPlanRunResult,
) -> Option<String> {
    live.run_plasm_meta
        .as_ref()
        .and_then(|m| serde_json::to_string(&serde_json::Value::Object(m.clone())).ok())
}

/// Agent-global engine state: one monotonic symbol registry per agent catalog universe.
pub struct AgentEngine {
    python_pool: Arc<plasm_agent_core::python_pool::PythonPool>,
    session_id: String,
    execute_session_id: plasm_agent_core::execute_path_ids::ExecuteSessionId,
    discovery_pin: Option<DiscoverySessionPin>,
    prerequisite_deployments: plasm_core::prerequisites::DeploymentBindings,
    intent: String,
    intent_turns: Vec<String>,
    workflow_config: NativeLearningConfig,
    host: Option<plasm_agent_core::server_state::PlasmHostState>,
    manifest_paths: Vec<std::path::PathBuf>,
    capabilities: Vec<(String, String)>,
    catalogs: IndexMap<String, Arc<CGS>>,
    compiled_catalogs: IndexMap<String, Arc<plasm_compile::CompiledCatalog>>,
    catalog_digests: IndexMap<String, String>,
    exposure: Option<TeachingExposureSession>,
    python_teaching: plasm_core::prompt_render::python::PythonTeachingState,
    pipeline: PromptPipelineConfig,
    sym_cross: SymbolMapCrossRequestCache,
    /// Execute session retaining graph state and reviewed plans across exposure waves.
    execute_session: Option<ExecuteSession>,
}

impl Default for AgentEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEngine {
    pub fn new() -> Self {
        Self {
            python_pool: Arc::new(Default::default()),
            session_id: plasm_agent_core::session_identity::LogicalSessionId::new_v4()
                .as_uuid()
                .to_string(),
            execute_session_id: plasm_agent_core::execute_path_ids::ExecuteSessionId::new_random(),
            discovery_pin: None,
            prerequisite_deployments: Default::default(),
            intent: String::new(),
            intent_turns: Vec::new(),
            workflow_config: NativeLearningConfig::Baseline,
            host: None,
            manifest_paths: Vec::new(),
            capabilities: Vec::new(),
            catalogs: IndexMap::new(),
            compiled_catalogs: IndexMap::new(),
            catalog_digests: IndexMap::new(),
            exposure: None,
            python_teaching: Default::default(),
            pipeline: PromptPipelineConfig::default(),
            sym_cross: SymbolMapCrossRequestCache::from_env(),
            execute_session: None,
        }
    }

    pub fn set_intent(&mut self, intent: impl Into<String>) {
        self.intent = intent.into();
        self.intent_turns = vec![self.intent.clone()];
    }

    pub(crate) fn set_python_pool(&mut self, pool: Arc<plasm_agent_core::python_pool::PythonPool>) {
        self.python_pool = pool;
    }

    pub fn configure_workflow_learning(&mut self, config: NativeLearningConfig) -> Result<()> {
        if self.host.is_some() || self.execute_session.is_some() {
            return Err(AgentEngineError::WorkflowAlreadyStarted);
        }
        config.validate()?;
        self.workflow_config = config;
        Ok(())
    }

    pub async fn completed_workflow_runs(&self) -> Result<String> {
        if self.workflow_config.mode() != NativeLearningMode::Collect {
            return Err(NativeLearningError::Mode.into());
        }
        match &self.host {
            Some(host) => Ok(serde_json::to_string(
                &self
                    .workflow_config
                    .completed_runs(host, &self.session_id, &self.intent)
                    .await?,
            )?),
            None => Ok(serde_json::json!({"intent":self.intent,"runs":[]}).to_string()),
        }
    }

    pub async fn read_workflow_evidence(&self, run_id: &str) -> Result<String> {
        let host = self
            .host
            .as_ref()
            .ok_or(AgentEngineError::WorkflowEvidenceMissing)?;
        Ok(serde_json::to_string(
            &self
                .workflow_config
                .read_evidence(host, &self.session_id, run_id)
                .await?,
        )?)
    }

    pub async fn review_workflow(&mut self, request: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Review {
            subtasks: Vec<plasm_agent_core::workflow_learning::model::AchievedSubtask>,
        }
        let request: Review = serde_json::from_str(request)?;
        let host = self
            .host
            .as_ref()
            .ok_or(AgentEngineError::WorkflowEvidenceMissing)?;
        let session = self
            .execute_session
            .as_ref()
            .ok_or(AgentEngineError::WorkflowEvidenceMissing)?;
        // Update the resident snapshot after native graph execution and exposure extension.
        self.workflow_config
            .register(
                host,
                session,
                &self.session_id,
                self.execute_session_id.as_str(),
                &self.intent_turns,
            )
            .await?;
        Ok(serde_json::to_string(
            &self
                .workflow_config
                .review(
                    host,
                    session,
                    &self.session_id,
                    self.execute_session_id.as_str(),
                    request.subtasks,
                )
                .await?,
        )?)
    }

    pub async fn fetch_workflow_example(&mut self, revision: &str) -> Result<String> {
        let host = match &self.host {
            Some(host) => host.clone(),
            None => self.build_host_state(None)?,
        };
        let session = self.ensure_execute_session()?;
        self.workflow_config
            .register(
                &host,
                &session,
                &self.session_id,
                self.execute_session_id.as_str(),
                &self.intent_turns,
            )
            .await?;
        let allowed = self
            .discovery_pin
            .as_ref()
            .map(|pin| pin.authorization.clone())
            .unwrap_or_else(|| {
                plasm_agent_core::discovery_store::DiscoveryAuthorization::catalogs(
                    self.catalogs.keys().cloned().collect(),
                )
            });
        let previous_capabilities = self
            .capabilities
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut served = self
            .workflow_config
            .fetch(
                &host,
                &session,
                self.execute_session_id.as_str(),
                revision,
                &allowed,
            )
            .await?;
        let updated = host
            .get_execute_session(AGENT_PROMPT_HASH, self.execute_session_id.as_str())
            .await
            .ok_or(AgentEngineError::WorkflowEvidenceMissing)?;
        self.exposure = updated.teaching_exposure.clone();
        if let Some(exposure) = &self.exposure {
            self.capabilities = exposure
                .entity_catalog_entry_ids
                .iter()
                .cloned()
                .zip(exposure.entities.iter().cloned())
                .collect();
        }
        self.execute_session = Some((*updated).clone());
        served.teaching_delta = self.render_teaching_delta(&[])?;
        #[derive(serde::Serialize)]
        struct NativeServedExample {
            #[serde(flatten)]
            example: plasm_agent_core::workflow_learning::serving::ServedExample,
            delta_refs: Vec<String>,
        }
        let delta_refs = self
            .capabilities
            .iter()
            .filter(|reference| !previous_capabilities.contains(*reference))
            .map(|(entry, entity)| format!("{entry}:{entity}"))
            .collect();
        Ok(serde_json::to_string(&NativeServedExample {
            example: served,
            delta_refs,
        })?)
    }

    pub fn introspect_catalog(&self, entry_id: &str) -> Result<CatalogIntrospection> {
        let cgs =
            self.catalogs
                .get(entry_id)
                .ok_or_else(|| AgentEngineError::CatalogNotLoaded {
                    entry_id: entry_id.to_owned(),
                })?;
        let digest = self
            .catalog_digests
            .get(entry_id)
            .cloned()
            .unwrap_or_else(|| cgs.catalog_cgs_hash_hex());

        let mut entities: Vec<EntityIntrospection> = cgs
            .entities
            .values()
            .map(|entity| EntityIntrospection {
                name: entity.name.to_string(),
                id_field: entity.id_field.to_string(),
                fields: entity
                    .fields
                    .values()
                    .map(|field| EntityFieldIntrospection {
                        name: field.name.to_string(),
                        value_ref: match &field.kind {
                            plasm_core::FieldValueKind::Registry(key) => key.as_str().to_string(),
                        },
                        required: field.required,
                    })
                    .collect(),
            })
            .collect();
        entities.sort_by(|a, b| a.name.cmp(&b.name));

        let names = entities
            .iter()
            .filter(|entity| {
                cgs.capabilities
                    .values()
                    .any(|cap| cap.domain.as_str() == entity.name)
            })
            .map(|entity| entity.name.as_str())
            .collect::<Vec<_>>();
        let exposure = plasm_core::TeachingExposureSession::new(cgs, entry_id, &names);
        let symbols = exposure.to_symbol_map();
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            &exposure,
            &Default::default(),
        )?;
        let mut capabilities: Vec<CapabilityIntrospection> = cgs
            .capabilities
            .values()
            .map(|cap| CapabilityIntrospection {
                name: cap.name.to_string(),
                kind: serde_json::to_value(cap.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{:?}", cap.kind).to_lowercase()),
                entity: cap.domain.to_string(),
                invoke_wire_name: capability_method_label_kebab(cap),
                python: PythonCapabilityBinding {
                    entity_symbol: exposure
                        .qualified_entity_symbol(entry_id, cap.domain.as_str())
                        .expect("exposed capability"),
                    method: plasm_core::prompt_render::python::capability_method_name(
                        cgs, &symbols, entry_id, cap,
                    ),
                    receiver: cap.requires_receiver()
                        && !matches!(
                            cap.kind,
                            plasm_core::CapabilityKind::Get
                                | plasm_core::CapabilityKind::Query
                                | plasm_core::CapabilityKind::Search
                        ),
                    identity: if (cap.kind == plasm_core::CapabilityKind::Get
                        && !cap.get_requires_identity_anchor(cgs))
                        || plasm_core::sole_nullary_singleton_get(cgs, cap.domain.as_str())
                            .is_some()
                    {
                        vec![]
                    } else {
                        let entity = cgs
                            .get_entity(cap.domain.as_str())
                            .expect("capability entity");
                        if entity.key_vars.len() > 1 {
                            entity.key_vars.iter().map(ToString::to_string).collect()
                        } else {
                            vec![entity.id_field.to_string()]
                        }
                    },
                    parameters: cap.input_fields().cloned().collect(),
                    unavailable: wave
                        .capabilities
                        .iter()
                        .find(|covered| covered.capability == cap.name.as_str())
                        .and_then(|covered| covered.unavailable.clone()),
                },
                inputs: cap.inputs.clone(),
                provides: cgs.effective_ordered_response_fields(cap),
                output_schema: cap.output_schema.clone(),
            })
            .collect();
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(CatalogIntrospection {
            entry_id: entry_id.to_string(),
            catalog_cgs_hash: digest,
            entities,
            values: cgs.values.clone(),
            capabilities,
        })
    }

    pub fn load_catalog(&mut self, catalog_path: &Path) -> Result<CatalogInfo> {
        if !plasm_core::catalog_il::is_catalog_manifest_path(catalog_path) {
            return Err(AgentEngineError::PackedManifestRequired);
        }
        let manifest = plasm_core::catalog_il::read_catalog_manifest(catalog_path)?;
        let cgs = plasm_core::catalog_il::load_catalog_artifact(
            catalog_path
                .parent()
                .ok_or(AgentEngineError::ManifestDirectoryMissing)?,
            &manifest,
        )?;
        let compiled = plasm_compile::load_compiled_catalog_artifact(
            catalog_path
                .parent()
                .ok_or(AgentEngineError::ManifestDirectoryMissing)?,
            &manifest,
            &cgs,
        )?;
        let entry_id = manifest.entry_id;
        let digest = cgs.catalog_cgs_hash_hex();
        if let Some(existing) = self.catalog_digests.get(&entry_id) {
            if existing != &digest {
                return Err(AgentEngineError::CatalogDigestChanged {
                    entry_id: entry_id.clone(),
                });
            }
            return Ok(CatalogInfo {
                entry_id,
                catalog_cgs_hash: digest,
            });
        }
        self.manifest_paths.push(catalog_path.to_path_buf());
        self.catalog_digests
            .insert(entry_id.clone(), digest.clone());
        self.compiled_catalogs
            .insert(entry_id.clone(), Arc::new(compiled));
        self.catalogs.insert(entry_id.clone(), Arc::new(cgs));
        self.invalidate_execute_session();
        Ok(CatalogInfo {
            entry_id,
            catalog_cgs_hash: digest,
        })
    }

    pub fn packed_manifests(&self) -> Vec<std::path::PathBuf> {
        self.manifest_paths.clone()
    }

    pub fn allowed_catalogs(&self) -> std::collections::BTreeSet<String> {
        self.catalogs.keys().cloned().collect()
    }

    pub fn from_generation(
        catalogs: std::collections::BTreeMap<String, CGS>,
        compiled_catalogs: std::collections::BTreeMap<String, Arc<plasm_compile::CompiledCatalog>>,
        session_id: String,
    ) -> Self {
        let mut engine = Self::new();
        engine.session_id = session_id;
        for (id, cgs) in catalogs {
            engine
                .catalog_digests
                .insert(id.clone(), cgs.catalog_cgs_hash_hex());
            engine.catalogs.insert(id, Arc::new(cgs));
        }
        engine.compiled_catalogs.extend(compiled_catalogs);
        engine
    }

    pub fn from_pinned_generation(
        catalogs: std::collections::BTreeMap<String, CGS>,
        compiled_catalogs: std::collections::BTreeMap<String, Arc<plasm_compile::CompiledCatalog>>,
        pin: DiscoverySessionPin,
        prerequisite_deployments: plasm_core::prerequisites::DeploymentBindings,
    ) -> Self {
        let mut engine = Self::from_generation(catalogs, compiled_catalogs, pin.pin_id.clone());
        engine.discovery_pin = Some(pin);
        engine.prerequisite_deployments = prerequisite_deployments;
        engine
    }

    pub fn discovery_pin(&self) -> Option<&DiscoverySessionPin> {
        self.discovery_pin.as_ref()
    }

    pub fn exposed_capabilities(&self) -> Vec<plasm_core::prerequisites::CapabilityRef> {
        self.exposure
            .as_ref()
            .map(|e| {
                e.surface
                    .capabilities
                    .iter()
                    .map(|c| plasm_core::prerequisites::CapabilityRef {
                        catalog: c.entry_id.clone(),
                        capability: c.capability.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn expose_routing(
        &mut self,
        intent: &str,
        closure: &plasm_core::prerequisites::PrerequisiteClosure,
    ) -> Result<TeachingExposureResult> {
        let mut intent_text = self.intent.clone();
        let mut intent_turns = self.intent_turns.clone();
        let mut capabilities = self.capabilities.clone();
        let mut staged_exposure = self.exposure.clone();
        let mut staged_session = self.execute_session.clone();
        if !intent_text.is_empty() {
            intent_text.push('\n');
        }
        intent_text.push_str(intent);
        intent_turns.push(intent.to_owned());
        let mut grouped = std::collections::BTreeMap::<String, Vec<String>>::new();
        for reference in closure
            .business
            .iter()
            .chain(&closure.input_sources)
            .chain(&closure.prerequisites)
        {
            grouped
                .entry(reference.catalog.clone())
                .or_default()
                .push(reference.capability.clone());
        }
        let mut touched = Vec::new();
        for (entry, selected) in grouped {
            let cgs = self
                .catalogs
                .get(&entry)
                .ok_or_else(|| AgentEngineError::PinnedCatalogMissing {
                    entry_id: entry.clone(),
                })?
                .clone();
            let delta = plasm_core::capability_exposure::selected_capability_surface(
                &cgs, &entry, &selected,
            )?;
            let entities: Vec<_> = delta
                .required
                .entities
                .iter()
                .map(|e| e.entity.to_string())
                .collect();
            for entity in &entities {
                if !capabilities
                    .iter()
                    .any(|(api, name)| api == &entry && name == entity)
                {
                    capabilities.push((entry.clone(), entity.clone()));
                }
            }
            touched.extend(delta.required.entities.iter().cloned());
            let refs = entities.iter().map(String::as_str).collect::<Vec<_>>();
            if let Some(exposure) = &mut staged_exposure {
                let layers = self.catalogs.values().map(Arc::as_ref).collect::<Vec<_>>();
                exposure.expose_surface(&layers, cgs, &entry, &refs, delta);
            } else {
                staged_exposure = Some(TeachingExposureSession::new_with_intent_delta(
                    &cgs, &entry, &refs, delta,
                ));
            }
        }
        let exposure = staged_exposure
            .as_ref()
            .cloned()
            .ok_or(AgentEngineError::RoutingExposureMissing)?;
        if let Some(session) = &mut staged_session {
            // Keep graph state and reviewed plans while appending symbols.
            session.entities = exposure.entities.clone();
            session.teaching_exposure = Some(exposure.clone());
            session.context_intent = Some(intent_text.clone());
            session.domain_revision += 1;
        }
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            &exposure,
            &self.python_teaching,
        )?;
        let mut prompt = wave.declarations;
        let declarations_changed = !prompt.is_empty();
        let catalogs = self
            .catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), cgs.as_ref()))
            .collect();
        let symbols = exposure.to_symbol_map();
        let guidance = plasm_core::prompt_render::render_prerequisite_bindings(
            closure,
            &catalogs,
            symbols.as_ref(),
        )?;
        if !guidance.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&guidance);
        }
        if self.workflow_config.mode() != NativeLearningMode::Baseline && staged_session.is_none() {
            staged_session =
                Some(self.build_execute_session_for(&capabilities, &exposure, &intent_text)?);
        }
        if let Some(session) = &mut staged_session {
            session.python_teaching = wave.next_state.clone();
            if self.workflow_config.mode() != NativeLearningMode::Baseline {
                let host = self.build_host_state(None)?;
                Box::pin(self.workflow_config.publish_exposure(
                    &host,
                    session,
                    &self.session_id,
                    self.execute_session_id.as_str(),
                    &intent_turns,
                ))
                .await?;
            }
        }
        // Publish local state only after the authoritative descriptor accepts the wave.
        self.intent = intent_text;
        self.intent_turns = intent_turns;
        self.capabilities = capabilities;
        self.exposure = staged_exposure;
        self.execute_session = staged_session;
        self.python_teaching = wave.next_state;
        Ok(TeachingExposureResult {
            prompt,
            declarations_changed,
            delta_refs: touched
                .iter()
                .map(|e| format!("{}:{}", e.entry_id, e.entity))
                .collect(),
        })
    }

    /// Explicit local execution may expose a caller-named entity without discovery.
    /// Every capability is selected by exact ownership; intent never controls admission.
    pub async fn expose_seeds(
        &mut self,
        intent: impl Into<String>,
        seeds: &[CapabilitySeed],
    ) -> Result<TeachingExposureResult> {
        let mut business = Vec::new();
        for seed in seeds {
            let cgs = self.catalogs.get(&seed.entry_id).ok_or_else(|| {
                AgentEngineError::SeedCatalogMissing {
                    entry_id: seed.entry_id.clone(),
                }
            })?;
            if !cgs.entities.contains_key(seed.entity.as_str()) {
                return Err(AgentEngineError::SeedEntityMissing {
                    entry_id: seed.entry_id.clone(),
                    entity: seed.entity.clone(),
                });
            }
            business.extend(
                cgs.capabilities
                    .values()
                    .filter(|cap| cap.domain.as_str() == seed.entity)
                    .map(|cap| plasm_core::prerequisites::CapabilityRef {
                        catalog: seed.entry_id.clone(),
                        capability: cap.name.to_string(),
                    }),
            );
        }
        self.expose_routing(
            &intent.into(),
            &plasm_core::prerequisites::PrerequisiteClosure {
                business,
                input_sources: vec![],
                prerequisites: vec![],
                acquisitions: vec![],
                edges: vec![],
            },
        )
        .await
    }

    fn reject_from_stage(
        &self,
        es: &ExecuteSession,
        program: &str,
        stage: ProgramStageError,
    ) -> DryRunResult {
        let failure = plasm_runtime::ExecutionFailure::from(stage.clone());
        let diag = ProgramDiagnostic::from_stage(
            &self.pipeline,
            Some(&self.sym_cross),
            es,
            program,
            stage,
        );
        DryRunResult {
            write_count: 0,
            failure_json: Some(serde_json::to_string(&failure).expect("typed failure serializes")),
            plan_commit_ref: String::new(),
            summary: diag.agent_markdown(),
            comp_json: serde_json::Value::Null,
            fused_clean_read: false,
        }
    }

    pub async fn dry_run(&mut self, program: &str) -> Result<DryRunResult> {
        let trimmed = program.trim();
        let es = self.ensure_execute_session()?;
        if trimmed.is_empty() {
            return Ok(self.reject_from_stage(
                &es,
                trimmed,
                ProgramStageError::Parse {
                    correction: "Submit a Python Program subclass with build(self).".into(),
                    span_offset: None,
                    error: std::sync::Arc::new(
                        plasm_agent_core::program_diagnostic::ProgramParseError::EmptyProgram,
                    ),
                },
            ));
        }
        let bundle = match Box::pin(compile_program(
            &self.pipeline,
            Some(&self.sym_cross),
            &es,
            "plasm_node",
            trimmed,
        ))
        .await
        {
            Ok(b) => b,
            Err(plasm_agent_core::compilation_error::CompilationError::Program(stage)) => {
                return Ok(self.reject_from_stage(&es, trimmed, *stage));
            }
            Err(
                error @ (plasm_agent_core::compilation_error::CompilationError::Host(_)
                | plasm_agent_core::compilation_error::CompilationError::Checker(_)),
            ) => {
                let failure: plasm_runtime::ExecutionFailure = error.into();
                return Ok(DryRunResult {
                    write_count: 0,
                    failure_json: Some(serde_json::to_string(&failure)?),
                    plan_commit_ref: String::new(),
                    summary: plasm_agent_core::agent_presentation::render_failure(&failure)
                        .into_string(),
                    comp_json: serde_json::Value::Null,
                    fused_clean_read: false,
                });
            }
        };
        let dry = match evaluate_plasm_comp_dry(&es, &bundle) {
            Ok(d) => d,
            Err(stage) => return Ok(self.reject_from_stage(&es, trimmed, stage)),
        };
        let fused_clean_read = dry.fuse_clean_read();
        let summary = render_plasm_plan_dry_text_for_session(&dry, None, Some(&es));
        let compact = plan_dry_compact_view(&dry, Some(&es));
        let commit_ref = es.mint_plan_commit_ref();
        let record = PlanCommitRecord::from_dry_review(
            commit_ref.clone(),
            compute_plan_commit_id_from_dry(&dry),
            es.domain_revision,
            &dry,
            trimmed.to_string(),
            compact.verdict,
            Instant::now() + PLAN_COMMIT_TTL,
        )
        .map_err(|denial| AgentEngineError::FlowDenial {
            verdict: format!("{:?}", denial.verdict),
            violations: denial.violations.len(),
        })?;
        es.register_plan_commit(record);
        self.execute_session = Some(es);
        Ok(DryRunResult {
            write_count: compact
                .write_count
                .try_into()
                .expect("bounded plan write count"),
            failure_json: None,
            plan_commit_ref: commit_ref.as_str().to_string(),
            summary,
            comp_json: serde_json::to_value(&bundle.artifact().comp)?,
            fused_clean_read,
        })
    }

    /// Validates `plan_commit_ref` against the in-process execute session.
    pub fn run_plan(&mut self, plan_commit_ref: &str) -> Result<RunPlanResult> {
        let trimmed = plan_commit_ref.trim();
        if trimmed.is_empty() {
            return Ok(RunPlanResult::reference_rejection(
                "invalid_run_reference",
                "Copy the reviewed commit reference from plasm or the continuation handle from plasm_run; artifact identifiers are not executable references.",
            ));
        }
        let Some(commit_ref) = PlanCommitRef::parse(trimmed) else {
            return Ok(RunPlanResult::reference_rejection(
                "invalid_run_reference",
                "Copy a reviewed commit reference from plasm verbatim.",
            ));
        };
        let es = self.ensure_execute_session()?;
        if let Err(error) = resolve_committed_plan(&es, &commit_ref) {
            return Ok(RunPlanResult::failure(error.into()));
        }
        Ok(RunPlanResult {
            ok: false,
            message: format!("Plan `{trimmed}` validated. Pass a HostTransportFn to execute live."),
            rows_json: None,
            meta_json: None,
            artifacts_json: None,
            failure_json: None,
        })
    }

    /// Resolve a committed plan and execute live via `plasm-runtime` with outbound HTTP routed
    /// through the host transport callback.
    pub async fn run_plan_live(
        &mut self,
        plan_commit_ref: &str,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<RunPlanResult> {
        // Bound the public native future's layout independently of the composed
        // authorization, execution and evidence protocols it drives.
        let operation: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<RunPlanResult>> + '_>,
        > = Box::pin(async move {
            let trimmed = plan_commit_ref.trim();
            if trimmed.is_empty() {
                return Ok(RunPlanResult::reference_rejection(
                    "invalid_run_reference",
                    "Copy a reviewed commit reference or paging handle verbatim.",
                ));
            }
            let es = self.ensure_execute_session()?;
            let (bundle, dry, authorization) = match ExecutableRunTarget::parse(trimmed) {
                Ok(ExecutableRunTarget::Commit(commit_ref)) => {
                    let committed = match resolve_committed_plan(&es, &commit_ref) {
                        Ok(committed) => committed,
                        Err(error) => return Ok(RunPlanResult::failure(error.into())),
                    };
                    let bundle = PlasmCompBundle::new(committed.artifact.clone())
                        .map_err(AgentEngineError::CommittedPlanArtifact)?
                        .with_source(committed.program.clone())
                        .with_compilation_symbols(committed.dry_cache.compilation_symbols.clone());
                    let dry = match dry_for_committed_plasm_run(&es, &bundle, &committed) {
                        Ok(dry) => dry,
                        Err(error) => return Ok(RunPlanResult::failure(error.into())),
                    };
                    (bundle, dry, Some(commit_ref))
                }
                Ok(ExecutableRunTarget::Page(handle)) => {
                    let bundle = match plasm_agent_core::mcp_server::compile_page_continuation(
                        &es, &handle, 0,
                    ) {
                        Ok(bundle) => bundle,
                        Err(failure) => return Ok(RunPlanResult::failure(failure)),
                    };
                    let dry = match evaluate_plasm_comp_dry(&es, &bundle) {
                        Ok(dry) => dry,
                        Err(error) => return Ok(RunPlanResult::failure(error.into())),
                    };
                    (bundle, dry, None)
                }
                Err(_) => {
                    return Ok(RunPlanResult::reference_rejection(
                        "invalid_run_reference",
                        format!(
                            "Run reference `{trimmed}` is not executable. Copy a reviewed commit reference from plasm or a continuation handle from plasm_run verbatim; run artifact IDs identify observations, not executable plans."
                        ),
                    ));
                }
            };

            let host = self.build_host_state(Some(transport))?;
            self.workflow_config
                .register(
                    &host,
                    &es,
                    &self.session_id,
                    self.execute_session_id.as_str(),
                    &self.intent_turns,
                )
                .await?;
            let execution = plasm_agent_core::plan_authorization::execute_authorized(
                &es,
                &host,
                authorization.as_ref(),
                Box::pin(run_plasm_comp(
                    &es,
                    &host,
                    AGENT_PROMPT_HASH,
                    self.execute_session_id.as_str(),
                    &bundle,
                    true,
                    None,
                    None,
                    Some(dry),
                    Some(plasm_agent_core::McpResultTransportPolicy {
                        artifact_access: plasm_agent_core::ArtifactAccessMode::DagCompute,
                        ..Default::default()
                    }),
                )),
            );
            let live_result = if self.workflow_config.mode() == NativeLearningMode::Collect {
                plasm_agent_core::workflow_learning::native::observe_execution(
                    &host,
                    &es,
                    self.execute_session_id.as_str(),
                    &bundle,
                    execution,
                )
                .await
            } else {
                execution.await
            };
            let live = match live_result {
                Ok(live) => live,
                Err(failure) => {
                    return Ok(RunPlanResult {
                        ok: false,
                        message: plasm_agent_core::agent_presentation::render_failure(&failure)
                            .into_string(),
                        rows_json: None,
                        meta_json: None,
                        artifacts_json: None,
                        failure_json: Some(serde_json::to_string(&failure)?),
                    });
                }
            };

            let publication: Result<RunPlanResult> = async {
                let message = live
                    .run_markdown
                    .clone()
                    .unwrap_or_else(|| format!("Live run completed for `{trimmed}`."));
                let mut artifacts = Vec::new();
                for artifact in &live.code_plan_run_artifacts {
                    let id =
                        plasm_agent_core::run_artifacts::RunArtifactId::from_wire(&artifact.run_id)
                            .ok_or(AgentEngineError::InvalidRunArtifactId)?;
                    let payload = host
                        .run_artifacts
                        .get_payload_result(AGENT_PROMPT_HASH, self.execute_session_id.as_str(), id)
                        .await
                        .map_err(AgentEngineError::RunArtifact)?
                        .ok_or(AgentEngineError::RunArtifactMissing)?;
                    artifacts.push(serde_json::json!({
                        "run_id": artifact.run_id,
                        "producer_binding": artifact.node_id,
                        "reusable": es.recorded_output_reusable(id).await,
                        "snapshot": serde_json::from_slice::<serde_json::Value>(&payload.bytes)?,
                    }));
                }
                Ok(RunPlanResult {
                    ok: true,
                    message,
                    rows_json: live_run_rows_json(&live)?,
                    meta_json: live_run_meta_json(&live),
                    failure_json: None,
                    artifacts_json: Some(serde_json::to_string(&artifacts)?),
                })
            }
            .await;
            match publication {
                Ok(result) => Ok(result),
                Err(error) => {
                    let mut failure = plasm_runtime::ExecutionFailure::new(
                        plasm_runtime::FailureCause::Runtime,
                        "run_publication_failed",
                        error.to_string(),
                    );
                    for step in &live.return_steps {
                        failure = failure.with_effects(&step.result.operations);
                    }
                    Ok(RunPlanResult {
                        ok: false,
                        message: plasm_agent_core::agent_presentation::render_failure(&failure)
                            .into_string(),
                        rows_json: None,
                        meta_json: None,
                        artifacts_json: None,
                        failure_json: Some(serde_json::to_string(&failure)?),
                    })
                }
            }
        });
        operation.await
    }

    fn build_host_state(
        &mut self,
        transport: Option<Arc<dyn HttpTransport>>,
    ) -> Result<plasm_agent_core::server_state::PlasmHostState> {
        let pairs: Vec<RegistryEntryPair> = self
            .catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), id.clone(), Vec::new(), cgs.clone()))
            .collect();
        if pairs.is_empty() {
            return Err(AgentEngineError::NoCatalogsLoaded);
        }
        let registry = CgsRegistry::from_pairs(pairs);
        let base_url = self
            .primary_entry_id()
            .and_then(|id| self.catalogs.get(&id))
            .map(|cgs| cgs.http_backend.clone())
            .filter(|b| !b.trim().is_empty());
        let mut config = ExecutionConfig {
            base_url,
            ..ExecutionConfig::default()
        };
        config.apply_http_env_overrides();
        if let Some(existing) = &self.host {
            let mut state = existing.clone();
            if let Some(transport) = transport {
                state.oss.engine =
                    Arc::new(ExecutionEngine::new_with_transport(config, transport, None));
            }
            return Ok(state);
        }
        let engine = ExecutionEngine::new(config.clone())?;
        let mut state = build_plasm_host_state_with_learning(
            PlasmHostBootstrap {
                engine,
                mode: ExecutionMode::Live,
                registry: Arc::new(registry),
                catalog_bootstrap: CatalogBootstrap::Fixed,
                incoming_auth: None,
                run_artifacts: Arc::new(RunArtifactStore::memory()),
                session_graph_persistence: None,
                oss_local_filesystem_defaults: false,
            },
            self.workflow_config.learning(),
        )?;
        state.oss.python_pool = self.python_pool.clone();
        // Retain shared session/evidence state, never the per-call JS callback:
        // a referenced NAPI threadsafe function keeps the Node event loop alive.
        self.host = Some(state.clone());
        if let Some(transport) = transport {
            state.oss.engine =
                Arc::new(ExecutionEngine::new_with_transport(config, transport, None));
        }
        Ok(state)
    }

    pub(crate) fn primary_entry_id(&self) -> Option<String> {
        primary_entry_id_from_seeds(
            &self
                .capabilities
                .iter()
                .map(|(api, entity)| CapabilitySeed {
                    entry_id: api.clone(),
                    entity: entity.clone(),
                })
                .collect::<Vec<_>>(),
        )
    }

    fn invalidate_execute_session(&mut self) {
        self.execute_session = None;
    }

    fn render_teaching_delta(
        &mut self,
        _all_new_qualified: &[ExposureEntityKey],
    ) -> Result<String> {
        let exposure = self
            .exposure
            .as_ref()
            .ok_or(AgentEngineError::TeachingExposureMissing)?;
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            exposure,
            &self.python_teaching,
        )?;
        // The tool description owns the static language reference. Context
        // waves contain only the append-only session declarations.
        let text = wave.declarations;
        self.python_teaching = wave.next_state;
        Ok(text)
    }

    fn build_execute_session(&self) -> Result<ExecuteSession> {
        let exposure = self
            .exposure
            .as_ref()
            .ok_or(AgentEngineError::TeachingExposureMissing)?;
        self.build_execute_session_for(&self.capabilities, exposure, &self.intent)
    }

    fn build_execute_session_for(
        &self,
        capabilities: &[(String, String)],
        exposure: &TeachingExposureSession,
        intent: &str,
    ) -> Result<ExecuteSession> {
        use plasm_core::CgsContext;

        let mut contexts_by_entry: IndexMap<String, Arc<CgsContext>> = IndexMap::new();
        for (api, cgs) in &self.catalogs {
            contexts_by_entry.insert(api.clone(), Arc::new(CgsContext::entry(api, cgs.clone())));
        }
        let seeds: Vec<CapabilitySeed> = capabilities
            .iter()
            .map(|(api, entity)| CapabilitySeed {
                entry_id: api.clone(),
                entity: entity.clone(),
            })
            .collect();
        let primary_api =
            primary_entry_id_from_seeds(&seeds).ok_or(AgentEngineError::NoCatalogsLoaded)?;
        if !self.catalogs.contains_key(&primary_api) {
            return Err(AgentEngineError::PrimaryCatalogMissing {
                entry_id: primary_api,
            });
        }
        let cgs = self
            .catalogs
            .get(&primary_api)
            .ok_or_else(|| AgentEngineError::PrimaryCatalogMissing {
                entry_id: primary_api.clone(),
            })?
            .clone();
        let exposure = exposure.clone();
        let entities = exposure.entities.clone();
        let catalog_cgs_hash = cgs.catalog_cgs_hash_hex();
        let mut session = ExecuteSession::new_with_bindings(
            AGENT_PROMPT_HASH.into(),
            self.session_id.clone(),
            cgs,
            contexts_by_entry,
            primary_api,
            self.workflow_config
                .library()
                .map(|c| c.scope.tenant.clone())
                .unwrap_or_default(),
            String::new(),
            None,
            entities,
            Some(exposure),
            None,
            catalog_cgs_hash,
            if intent.is_empty() {
                None
            } else {
                Some(intent.to_owned())
            },
            IndexMap::new(),
            self.compiled_catalogs.clone(),
        );
        session.discovery_pin = self.discovery_pin.clone();
        session.python_teaching = self.python_teaching.clone();
        session.set_prerequisite_deployments(self.prerequisite_deployments.clone());
        Ok(session)
    }

    fn ensure_execute_session(&mut self) -> Result<ExecuteSession> {
        if let Some(es) = self.execute_session.clone() {
            return Ok(es);
        }
        let es = self.build_execute_session()?;
        self.execute_session = Some(es.clone());
        Ok(es)
    }
}

fn primary_entry_id_from_seeds(seeds: &[CapabilitySeed]) -> Option<String> {
    let mut ids: Vec<String> = seeds.iter().map(|s| s.entry_id.clone()).collect();
    ids.sort();
    ids.dedup();
    ids.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_agent_core::http_execute::PublishedResultStep;
    use plasm_agent_core::plasm_plan_run::PlasmPlanRunResult;
    use plasm_core::{EntityKey, Ref, Value};
    use plasm_runtime::{
        CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource, ExecutionStats,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn run_reference_admission_is_effect_free_and_correctable() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "abstract reference admission",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        for reference in [
            "",
            "????",
            "pr0000000000000000000000000000000000000000000000000000000000000000",
            "pc999",
        ] {
            let result = engine
                .run_plan(reference)
                .expect("admission is a typed observation");
            let failure: serde_json::Value =
                serde_json::from_str(result.failure_json.as_ref().unwrap()).unwrap();
            assert_eq!(failure["cause"], "program");
            assert_eq!(failure["recovery"], "repair_program");
            assert_eq!(failure["effects_unresolved"], false);
            assert_eq!(failure["effects"], serde_json::json!([]));
            assert_eq!(failure["dispatches"], serde_json::json!([]));
        }
    }

    #[tokio::test]
    async fn stale_policy_admission_is_correctable_and_never_dispatches() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "policy admission",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
            .await
            .unwrap();
        let es = engine.ensure_execute_session().unwrap();
        let reference = PlanCommitRef::parse(&dry.plan_commit_ref).unwrap();
        let mut record = es.get_plan_commit(&reference).unwrap();
        record.policy_revision = plasm_agent_core::PolicyRevision(99);
        es.register_plan_commit(record);
        let rejection = engine
            .run_plan_live(&dry.plan_commit_ref, Arc::new(AdmissionMustNotDispatch))
            .await
            .unwrap();
        let failure: plasm_runtime::ExecutionFailure =
            serde_json::from_str(rejection.failure_json.as_ref().unwrap()).unwrap();
        assert_eq!(failure.code, "plan_commit_stale_policy");
        assert_eq!(
            failure.recovery,
            plasm_runtime::RecoveryDisposition::RepairProgram
        );
        assert!(
            failure.effects.is_empty()
                && failure.dispatches.is_empty()
                && !failure.effects_unresolved
        );
        let rejection = engine.run_plan(&dry.plan_commit_ref).unwrap();
        assert_eq!(
            rejection.failure_json,
            Some(serde_json::to_string(&failure).unwrap())
        );
    }

    #[tokio::test]
    async fn live_run_reference_admission_rejects_unknown_and_expired_before_dispatch() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "abstract reference admission",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
            .await
            .unwrap();
        let es = engine.ensure_execute_session().unwrap();
        let reference = PlanCommitRef::parse(&dry.plan_commit_ref).unwrap();
        let mut record = es.get_plan_commit(&reference).unwrap();
        record.expires_at = Instant::now();
        es.register_plan_commit(record);
        for reference in [
            "",
            "????",
            "pr0000000000000000000000000000000000000000000000000000000000000000",
            "pc999",
            &dry.plan_commit_ref,
        ] {
            let result = engine
                .run_plan_live(reference, Arc::new(AdmissionMustNotDispatch))
                .await
                .unwrap();
            let failure: plasm_runtime::ExecutionFailure =
                serde_json::from_str(result.failure_json.as_ref().unwrap()).unwrap();
            assert_eq!(
                failure.recovery,
                plasm_runtime::RecoveryDisposition::RepairProgram
            );
            assert!(!failure.effects_unresolved);
            assert!(failure.effects.is_empty() && failure.dispatches.is_empty());
        }
    }

    struct AdmissionMustNotDispatch;
    #[async_trait::async_trait]
    impl HttpTransport for AdmissionMustNotDispatch {
        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<
            (serde_json::Value, Option<String>),
            plasm_runtime::error::RuntimeError,
        > {
            panic!("rejected admission must never fetch");
        }
        async fn send_compiled_http(
            &self,
            _: &str,
            _: &plasm_compile::CompiledRequest,
            _: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<
            (serde_json::Value, Option<String>),
            plasm_runtime::error::RuntimeError,
        > {
            panic!("rejected admission must never dispatch");
        }
    }

    #[test]
    fn agent_engine_error_has_bounded_footprint() {
        let size = std::mem::size_of::<AgentEngineError>();
        assert!(size < 128, "AgentEngineError occupies {size} bytes");
    }

    #[test]
    fn boxed_catalog_import_retains_concrete_source_chain() {
        use plasm_core::catalog_il::CatalogIlError;
        use std::error::Error;

        let error = AgentEngineError::from(CatalogIlError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "catalog fixture denied",
        )));
        assert_eq!(error.to_string(), "catalog interchange failed");
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<CatalogIlError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source, CatalogIlError::Io(_)));
        let io = source
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(io.to_string(), "catalog fixture denied");
    }

    #[test]
    fn boxed_paging_import_retains_stage_metadata_and_source() {
        use std::error::Error;

        let stage = ProgramStageError::CoreType {
            error: plasm_core::TypeError::FieldNotFound {
                field: "missing".into(),
                entity: "Product".into(),
            },
        };
        let expected_display = format!("dry evaluation of paging plan failed: {stage}");
        let error = AgentEngineError::from(stage);
        assert_eq!(error.to_string(), expected_display);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<ProgramStageError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source, ProgramStageError::CoreType { .. }));
        let cause = source
            .source()
            .unwrap()
            .downcast_ref::<plasm_core::TypeError>()
            .unwrap();
        assert!(
            matches!(cause, plasm_core::TypeError::FieldNotFound { field, entity }
            if field == "missing" && entity == "Product")
        );
    }

    fn execute_tiny_dir() -> PathBuf {
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("test manifest directory"))
            .join("../plasm-agent-core/tests/fixtures/execute_tiny")
    }

    fn synthetic_live_run_result() -> PlasmPlanRunResult {
        synthetic_live_run_result_with_name(Value::String("Widget".into()))
    }

    fn synthetic_live_run_result_with_name(name: Value) -> PlasmPlanRunResult {
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("p1".into()));
        fields.insert("name".into(), name);
        let entity = CachedEntity::from_decoded(
            Ref {
                entity_type: "Product".into(),
                key: EntityKey::Simple("p1".into()),
            },
            fields,
            IndexMap::new(),
            0,
            EntityCompleteness::Complete,
        );
        let step = PublishedResultStep {
            source_heading: None,
            name: None,
            node_id: None,
            entry_id: None,
            entity: Some("Product".into()),
            cgs: None,
            display: "products".into(),
            projection: None,
            result: Arc::new(ExecutionResult {
                collection: plasm_runtime::execution::ExecutionCollection::observe(
                    plasm_core::collection_codec::CollectionIdentity::for_untyped_observation(
                        &"node_fixture",
                    )
                    .unwrap(),
                    vec![entity],
                    plasm_core::collection_codec::Observation::UnprovenPage,
                )
                .unwrap(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec!["abc123".into()],
                operations: plasm_runtime::OperationLedger::empty(),
            }),
            artifact: None,
        };
        let mut meta = serde_json::Map::new();
        let mut plasm = serde_json::Map::new();
        plasm.insert(
            "steps".into(),
            serde_json::json!([{ "request_fingerprints": ["abc123"] }]),
        );
        meta.insert("plasm".into(), serde_json::Value::Object(plasm));
        PlasmPlanRunResult {
            version: serde_json::json!(1),
            agent_outcome: Default::default(),
            node_results: Vec::new(),
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some("## done".into()),
            run_plasm_meta: Some(meta),
            return_steps: vec![step],
            inline_plan_ui: None,
        }
    }

    fn tiny_engine() -> (AgentEngine, CatalogInfo) {
        let mut cgs = plasm_core::load_schema_dir(&execute_tiny_dir()).unwrap();
        cgs.bind_registry_entry_id("matrix");
        let info = CatalogInfo {
            entry_id: "matrix".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        (
            AgentEngine::from_generation(
                [("matrix".into(), cgs)].into(),
                [("matrix".into(), compiled)].into(),
                "matrix-session".into(),
            ),
            info,
        )
    }

    #[tokio::test]
    async fn obligation_verifier_native_artifact_contract() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "verify observed resources",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        let dry = engine.dry_run("class Verify(Program):\n    @compute\n    def check(self, rows: list[Row]) -> bool:\n        return len(rows) == 1\n    def build(self):\n        observed = e1.query()\n        verified = self.check(observed)\n        return verified\n").await.unwrap();
        assert!(dry.failure_json.is_none(), "{}", dry.summary);
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap();
        assert!(result.ok, "{}", result.message);
        let repeated = engine
            .run_plan_live(&dry.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap();
        assert!(
            !repeated.ok,
            "consumed authorization must not execute again"
        );
        let fresh = engine.dry_run("class Verify(Program):\n    @compute\n    def check(self, rows: list[Row]) -> bool:\n        return len(rows) == 1\n    def build(self):\n        observed = e1.query()\n        verified = self.check(observed)\n        return verified\n").await.unwrap();
        assert_ne!(dry.plan_commit_ref, fresh.plan_commit_ref);
        assert_eq!(dry.comp_json, fresh.comp_json);
        let observed_again = engine
            .run_plan_live(&fresh.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap();
        assert!(observed_again.ok, "{}", observed_again.message);
        let fixture = serde_json::json!({
            "comp": dry.comp_json,
            "artifacts": serde_json::from_str::<serde_json::Value>(result.artifacts_json.as_ref().unwrap()).unwrap(),
            "meta": serde_json::from_str::<serde_json::Value>(result.meta_json.as_ref().unwrap()).unwrap(),
        });
        if let Some(path) = std::env::var_os("PLASM_OBLIGATION_FIXTURE_OUT") {
            std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
        }
        engine.python_pool.close().await;
    }

    fn return_projection_engine() -> (AgentEngine, CatalogInfo) {
        let dir =
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("test manifest directory"))
                .join("../../fixtures/schemas/return_projection_teaching");
        let mut cgs = plasm_core::load_schema_dir(&dir).unwrap();
        cgs.bind_registry_entry_id("return_projection");
        let info = CatalogInfo {
            entry_id: "return_projection".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        (
            AgentEngine::from_generation(
                [("return_projection".into(), cgs)].into(),
                [("return_projection".into(), compiled)].into(),
                "return-projection-session".into(),
            ),
            info,
        )
    }

    #[tokio::test]
    async fn introspection_bindings_match_full_python_exposure() {
        let (mut engine, _) = tiny_engine();
        let catalog = engine.introspect_catalog("matrix").unwrap();
        let seeds = catalog
            .entities
            .iter()
            .filter(|entity| {
                catalog
                    .capabilities
                    .iter()
                    .any(|cap| cap.entity == entity.name)
            })
            .map(|entity| CapabilitySeed {
                entry_id: "matrix".into(),
                entity: entity.name.clone(),
            })
            .collect::<Vec<_>>();
        let teaching = engine.expose_seeds("", &seeds).await.unwrap();
        assert!(!teaching.prompt.contains("Program.build:"));
        assert!(!teaching
            .prompt
            .contains("Declarations: eN/mN/rN/vN session-local/append-only"));
        for cap in &catalog.capabilities {
            if cap.python.unavailable.is_some() {
                continue;
            }
            assert!(
                teaching.prompt.contains(&format!(
                    "  {}.{}(",
                    if cap.python.receiver {
                        "row"
                    } else {
                        &cap.python.entity_symbol
                    },
                    cap.python.method
                )),
                "missing {}",
                cap.name
            );
            let exposure = engine.exposure.as_ref().unwrap();
            assert_eq!(
                exposure
                    .qualified_entity_symbol("matrix", &cap.entity)
                    .unwrap(),
                cap.python.entity_symbol
            );
            let cgs = &engine.catalogs["matrix"];
            assert_eq!(
                plasm_core::prompt_render::python::capability_method_name(
                    cgs,
                    &exposure.to_symbol_map(),
                    "matrix",
                    &cgs.capabilities[cap.name.as_str()]
                ),
                cap.python.method
            );
        }
    }

    #[tokio::test]
    async fn load_expose_and_dry_run_execute_tiny() {
        let (mut engine, info) = tiny_engine();
        assert!(!info.catalog_cgs_hash.is_empty());
        let teaching = engine
            .expose_seeds(
                "test intent",
                &[CapabilitySeed {
                    entry_id: info.entry_id.clone(),
                    entity: "Product".into(),
                }],
            )
            .await
            .expect("expose");
        assert!(teaching.prompt.contains("e1") || !teaching.prompt.is_empty());
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
            .await
            .expect("dry run");
        assert!(dry.plan_commit_ref.starts_with("pc"));
        assert!(!dry.summary.is_empty());
        assert!(
            dry.fused_clean_read,
            "read-only Product query must follow the shared clean-read verdict: {}",
            dry.summary
        );
    }

    #[tokio::test]
    async fn execution_failure_missing_worker_is_not_a_node_program_correction() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "test intent",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        engine.set_python_pool(Arc::new(
            plasm_agent_core::python_pool::PythonPool::with_binary(std::path::PathBuf::from(
                "/nonexistent-plasm-conformance-worker",
            )),
        ));
        let dry = engine.dry_run("class Read(Program):\n    @compute\n    def count(self, rows: list[Row]) -> int:\n        return len(rows)\n    def build(self):\n        return self.count(e1.query())\n").await.unwrap();
        assert!(dry.failure_json.is_none());
        assert!(!dry.plan_commit_ref.is_empty());
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap();
        assert!(!result.ok);
        let failure: plasm_runtime::ExecutionFailure = serde_json::from_str(
            result
                .failure_json
                .as_ref()
                .expect("host-owned failure envelope"),
        )
        .unwrap();
        assert_eq!(failure.cause, plasm_runtime::FailureCause::Runtime);
        assert_eq!(failure.recovery, plasm_runtime::RecoveryDisposition::Stop);
        assert_eq!(failure.code, "python_pool_failure");
        assert!(!result.message.contains("/nonexistent"));
        assert!(!result.message.contains("class Read"));
        engine.python_pool.close().await;
    }

    fn program_correction(rejection: DryRunResult) -> String {
        let failure: plasm_runtime::ExecutionFailure =
            serde_json::from_str(rejection.failure_json.as_ref().expect("typed rejection"))
                .unwrap();
        assert_eq!(failure.cause, plasm_runtime::FailureCause::Program);
        assert_eq!(
            failure.recovery,
            plasm_runtime::RecoveryDisposition::RepairProgram
        );
        assert!(rejection.plan_commit_ref.is_empty());
        rejection.summary
    }

    #[tokio::test]
    async fn identical_dry_run_reject_names_already_rejected() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "test intent",
                &[CapabilitySeed {
                    entry_id: info.entry_id.clone(),
                    entity: "Product".into(),
                }],
            )
            .await
            .expect("expose");
        let program = "class Read(Program):\n    def build(self):\n        return e1.query().select(\"not_a_taught_field\")\n";
        let first = engine
            .dry_run(program)
            .await
            .map(program_correction)
            .expect("structured program rejection");
        assert!(
            first.contains("not_a_taught_field"),
            "first reject names the field: {first}"
        );
        assert!(
            !first.contains("already rejected"),
            "first reject is fresh: {first}"
        );
        let second = engine
            .dry_run(program)
            .await
            .map(program_correction)
            .expect("structured program rejection");
        assert!(
            second.contains("This exact program was already rejected"),
            "NAPI dry_run must record through from_stage: {second}"
        );
    }

    #[tokio::test]
    async fn identical_parse_reject_names_already_rejected() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "test intent",
                &[CapabilitySeed {
                    entry_id: info.entry_id.clone(),
                    entity: "Product".into(),
                }],
            )
            .await
            .expect("expose");
        let program = "note = e1(3084, access_token=missing)";
        let first = engine
            .dry_run(program)
            .await
            .map(program_correction)
            .expect("structured program rejection");
        assert!(
            !first.contains("already rejected"),
            "first parse reject is fresh: {first}"
        );
        let second = engine
            .dry_run(program)
            .await
            .map(program_correction)
            .expect("structured program rejection");
        assert!(
            second.contains("This exact program was already rejected"),
            "NAPI parse reject must record through from_stage: {second}"
        );
    }

    /// Official `synthesizeTeaching` is `exposeSeeds` → `AgentEngine::expose_seeds`.
    /// RA-12: Get/Query `[…]` is the full authored set, not a `provides` subset.
    #[tokio::test]
    async fn expose_seeds_teaches_ra12_return_projection_not_provides_subset() {
        let (mut engine, info) = return_projection_engine();
        let teaching = engine
            .expose_seeds(
                "review transfers and notices",
                &[
                    CapabilitySeed {
                        entry_id: info.entry_id.clone(),
                        entity: "Notice".into(),
                    },
                    CapabilitySeed {
                        entry_id: info.entry_id.clone(),
                        entity: "Transaction".into(),
                    },
                ],
            )
            .await
            .expect("exposeSeeds");
        for field in [
            "notice_id",
            "author_email",
            "body",
            "created_at",
            "title",
            "transaction_id",
            "amount",
            "description",
            "private",
        ] {
            assert!(
                teaching.prompt.contains(&format!("{field}:")),
                "missing typed field {field}: {}",
                teaching.prompt
            );
        }
        assert!(
            teaching.prompt.contains("author_email")
                && teaching.prompt.contains("created_at")
                && teaching.prompt.contains("private"),
            "sheared decode fields must appear on the exposeSeeds card:\n{}",
            teaching.prompt
        );
    }

    #[test]
    fn live_run_serialization_populates_rows_and_meta() {
        let live = synthetic_live_run_result();
        let rows = live_run_rows_json(&live)
            .expect("encode rows")
            .expect("rows_json");
        let parsed: serde_json::Value = serde_json::from_str(&rows).expect("parse result envelope");
        assert_eq!(parsed["rows"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["rows"][0]["id"], "p1");
        let meta = live_run_meta_json(&live).expect("meta_json");
        assert!(meta.contains("plasm"));
        assert!(meta.contains("abc123"));
    }

    #[test]
    fn live_run_serialization_checks_each_return_step() {
        let mut live = synthetic_live_run_result();
        live.return_steps
            .push(synthetic_live_run_result().return_steps.remove(0));
        let rows = live_run_rows_json(&live).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rows).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);

        let invalid = synthetic_live_run_result_with_name(Value::Float(f64::NAN));
        assert!(matches!(
            live_run_rows_json(&invalid),
            Err(AgentEngineError::ValueCodec(_))
        ));
        live.return_steps
            .push(invalid.return_steps.into_iter().next().unwrap());
        assert!(matches!(
            live_run_rows_json(&live),
            Err(AgentEngineError::ValueCodec(_))
        ));
        live.return_steps.clear();
        assert_eq!(live_run_rows_json(&live).unwrap(), None);
    }

    struct MockProductListTransport;

    #[async_trait::async_trait]
    impl plasm_runtime::http_transport::HttpTransport for MockProductListTransport {
        async fn send_compiled_http(
            &self,
            base_url: &str,
            request: &plasm_compile::CompiledRequest,
            _auth: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<
            (serde_json::Value, Option<String>),
            plasm_runtime::error::RuntimeError,
        > {
            assert!(
                !base_url.contains("localhost:3000"),
                "expected catalog http_backend, got {base_url}"
            );
            let path = request.url_path();
            if path.contains("/products/") && path != "/products" && !path.contains("/search") {
                return Ok((
                    serde_json::json!({"id": "p1", "name": "Widget", "category_id": "c1"}),
                    None,
                ));
            }
            Ok((
                serde_json::json!([{"id": "p1", "name": "Widget", "category_id": "c1"}]),
                None,
            ))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<
            (serde_json::Value, Option<String>),
            plasm_runtime::error::RuntimeError,
        > {
            Err(plasm_runtime::error::RuntimeError::LiveAbsolutePaginationRequired)
        }
    }

    #[test]
    fn export_native_workflow_fixture() {
        let Some(directory) = std::env::var_os("PLASM_NATIVE_LEARNING_FIXTURE_OUT") else {
            return;
        };
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let (engine, _) = tiny_engine();
        let cgs = engine.catalogs["matrix"].as_ref();
        let compiled = engine.compiled_catalogs["matrix"].as_ref();
        let write =
            |name: &str, bytes: Vec<u8>| std::fs::write(directory.join(name), bytes).unwrap();
        write("matrix.cgs.json", serde_json::to_vec(cgs).unwrap());
        let recipes = serde_json::to_vec(compiled).unwrap();
        let recipes_hash = plasm_core::catalog_discovery::content_hash(&recipes);
        write("matrix.recipes.json", recipes);
        let profile = plasm_core::catalog_discovery::EmbeddingProfile::default();
        let discovery = plasm_core::catalog_discovery::CatalogDiscoveryArtifact {
            entry_id: "matrix".into(),
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
            capabilities: plasm_core::catalog_discovery::capability_documents(cgs)
                .unwrap()
                .into_iter()
                .enumerate()
                .map(|(index, document)| {
                    let mut embedding = vec![0.0; profile.dimensions];
                    embedding[index % profile.dimensions] = 1.0;
                    plasm_core::catalog_discovery::EmbeddedCapability {
                        document,
                        embedding,
                    }
                })
                .collect(),
            profile: profile.clone(),
            prerequisites: cgs.prerequisites.clone(),
        };
        discovery.validate(cgs).unwrap();
        let discovery = serde_json::to_vec(&discovery).unwrap();
        let manifest = plasm_core::catalog_il::CatalogManifest {
            format_version: 3,
            entry_id: "matrix".into(),
            version: cgs.version,
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            label: "Native workflow fixture".into(),
            tags: vec![],
            cgs_json: "matrix.cgs.json".into(),
            recipes_json: "matrix.recipes.json".into(),
            recipes_hash,
            discovery_json: "matrix.discovery.json".into(),
            discovery_hash: plasm_core::catalog_discovery::content_hash(&discovery),
            embedding_profile: plasm_core::catalog_discovery::EmbeddingProfile::default(),
        };
        write("matrix.discovery.json", discovery);
        write(
            "matrix.manifest.json",
            serde_json::to_vec_pretty(&manifest).unwrap(),
        );
    }

    #[tokio::test]
    async fn native_collect_seals_executed_source_and_scoped_receipts() {
        let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
            return;
        };
        let (mut engine, info) = tiny_engine();
        engine.session_id =
            plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string();
        let config = NativeLearningConfig::Collect(
            plasm_agent_core::workflow_learning::native::NativeLibraryConfig {
                scope: plasm_agent_core::workflow_learning::model::LibraryScope {
                    tenant: format!("native-{}", engine.session_id),
                    project: "offline".into(),
                },
                database_url: url,
            },
        );
        engine.configure_workflow_learning(config.clone()).unwrap();
        engine
            .expose_seeds(
                "Inspect fixture products",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
            .await
            .unwrap();
        let source = "class Inspect(Program):\n    def build(self):\n        return e1.query()\n";
        let dry = engine.dry_run(source).await.unwrap();
        assert!(dry.fused_clean_read);
        let transport = Arc::new(MockProductListTransport);
        let released_transport = Arc::downgrade(&transport);
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, transport)
            .await
            .unwrap();
        assert!(result.ok, "{}", result.message);
        assert!(
            released_transport.upgrade().is_none(),
            "persistent evidence host must release per-call transport"
        );
        // Serial native context waves between runs must each publish their successor.
        for capability in ["category_get", "product_search", "product_list"] {
            engine
                .expose_routing(
                    "extend fixture teaching",
                    &plasm_core::prerequisites::PrerequisiteClosure {
                        business: vec![plasm_core::prerequisites::CapabilityRef {
                            catalog: "matrix".into(),
                            capability: capability.into(),
                        }],
                        input_sources: vec![],
                        prerequisites: vec![],
                        acquisitions: vec![],
                        edges: vec![],
                    },
                )
                .await
                .unwrap();
        }
        let next = engine.dry_run(source).await.unwrap();
        let result = engine
            .run_plan_live(&next.plan_commit_ref, Arc::new(MockProductListTransport))
            .await
            .unwrap();
        assert!(result.ok, "{}", result.message);
        let index: serde_json::Value =
            serde_json::from_str(&engine.completed_workflow_runs().await.unwrap()).unwrap();
        let runs = index["runs"].as_array().unwrap();
        assert!(!runs.is_empty());
        let run_id = runs[0]["run_id"].as_str().unwrap();
        let receipt: serde_json::Value =
            serde_json::from_str(&engine.read_workflow_evidence(run_id).await.unwrap()).unwrap();
        assert_eq!(receipt["source"], source.trim());
        assert_eq!(receipt["scope"]["tenant"], config.scope().unwrap().tenant);
        let review = serde_json::json!({"subtasks":[{"intent":"Inspect fixture products","observed_outcome":"Observed fixture product p1","run_ids":[run_id]}]});
        let sealed: serde_json::Value =
            serde_json::from_str(&engine.review_workflow(&review.to_string()).await.unwrap())
                .unwrap();
        assert!(sealed["review_ref"].is_string());
        let invalid = serde_json::json!({"subtasks":[{"intent":"Unobserved","observed_outcome":"No evidence","run_ids":["missing"]}]});
        assert!(engine.review_workflow(&invalid.to_string()).await.is_err());
        let repository = config.learning().repository().await.unwrap();
        let session_id = engine.session_id.clone();
        engine.python_pool.close().await;
        drop(engine);
        // Durable receipts remain after the producing native engine is gone.
        assert!(!repository
            .execution_receipts(
                &config.scope().unwrap(),
                plasm_agent_core::session_identity::LogicalSessionId::parse(&session_id).unwrap()
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn live_run_execute_tiny_returns_rows_json() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "list products",
                &[CapabilitySeed {
                    entry_id: info.entry_id.clone(),
                    entity: "Product".into(),
                }],
            )
            .await
            .expect("expose");
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        observed = e1.query()\n        return observed\n")
            .await
            .expect("dry run");
        let transport = Arc::new(MockProductListTransport);
        let live = engine
            .run_plan_live(&dry.plan_commit_ref, transport)
            .await
            .expect("live run");
        assert!(live.ok, "{}", live.message);
        let rows = live
            .rows_json
            .as_deref()
            .expect("rows_json should be populated");
        let parsed: serde_json::Value = serde_json::from_str(rows).expect("rows json");
        let arr = parsed["rows"]
            .as_array()
            .expect("entity rows in result envelope");
        assert!(!arr.is_empty());
        assert_eq!(arr[0]["id"], "p1");
        let artifacts: serde_json::Value =
            serde_json::from_str(live.artifacts_json.as_deref().expect("canonical artifacts"))
                .expect("artifact JSON");
        let artifacts = artifacts.as_array().expect("artifact array");
        assert!(!artifacts.is_empty());
        for artifact in artifacts {
            let run_id = artifact["run_id"].as_str().expect("native run id");
            assert!(plasm_agent_core::run_artifacts::RunArtifactId::from_wire(run_id).is_some());
            assert!(artifact["snapshot"].is_object());
            assert!(artifact["producer_binding"].is_string());
            assert_eq!(artifact["reusable"], true);
        }
        assert!(artifacts
            .iter()
            .any(|artifact| artifact["producer_binding"] == "observed"));
    }

    struct RecordingItemTransport(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl HttpTransport for RecordingItemTransport {
        async fn send_compiled_http(
            &self,
            _base_url: &str,
            request: &plasm_compile::CompiledRequest,
            _auth: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<(serde_json::Value, Option<String>), plasm_runtime::RuntimeError>
        {
            let path = request.url_path();
            if path.ends_with("/p1") || path.ends_with("/p2") {
                if request.method_str() == "PATCH" {
                    self.0.lock().unwrap().push(path.to_string());
                }
                return Ok((
                    serde_json::json!({"id":path.rsplit('/').next().unwrap(),"title":"checked","score":2,"owner":"alice"}),
                    None,
                ));
            }
            assert_eq!(path, "/language/v1/items");
            Ok((
                serde_json::json!([
                    {"id":"p1", "title":"First", "score":1, "owner":"alice"},
                    {"id":"p2", "title":"Second", "score":1, "owner":"alice"}
                ]),
                None,
            ))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<(serde_json::Value, Option<String>), plasm_runtime::RuntimeError>
        {
            panic!("unexpected absolute request");
        }
    }

    #[test]
    fn native_live_fanout_survives_two_mib_stack() {
        std::thread::Builder::new().stack_size(2 * 1024 * 1024).spawn(|| {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                .block_on(async {
                    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("test manifest directory"))
                        .join("../../fixtures/schemas/plasm_language_matrix");
                    let mut cgs = plasm_core::load_schema_dir(&dir).unwrap();
                    cgs.bind_registry_entry_id("matrix");
                    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
                    let mut engine = AgentEngine::from_generation(
                        [("matrix".into(), cgs)].into(), [("matrix".into(), compiled)].into(),
                        "stack-session".into(),
                    );
                    engine.expose_seeds("update items", &[CapabilitySeed {
                        entry_id: "matrix".into(), entity: "LangItem".into(),
                    }]).await.unwrap();
                    let cgs = &engine.catalogs["matrix"];
                    let symbol = plasm_core::prompt_render::python::capability_method_name(cgs, &engine.exposure.as_ref().unwrap().to_symbol_map(), "matrix", &cgs.capabilities["langitem_update"]);
                    let dry = engine.dry_run(&format!("class Update(Program):\n    def build(self):\n        return e1.query().flat_map(lambda row: row.{symbol}(title=\"checked\", score=2, owner=\"alice\"))\n")).await.unwrap();
                    let transport = Arc::new(RecordingItemTransport(std::sync::Mutex::new(Vec::new())));
                    let live = engine.run_plan_live(&dry.plan_commit_ref, transport.clone()).await.unwrap();
                    assert!(live.ok, "{}", live.message);
                    assert_eq!(*transport.0.lock().unwrap(), ["/language/v1/items/p1", "/language/v1/items/p2"]);
                });
        }).unwrap().join().unwrap();
    }

    #[tokio::test]
    async fn pinned_native_generation_preserves_prerequisite_deployments_in_each_wave() {
        use plasm_core::prerequisites::{CapabilityRef, DeploymentBinding, DeploymentBindings};
        let directory = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../fixtures/schemas/prerequisite_dual_session");
        let mut catalogs = std::collections::BTreeMap::<String, CGS>::new();
        let mut compiled = std::collections::BTreeMap::new();
        for entry in ["consumer", "source"] {
            let mut cgs = plasm_core::load_schema_dir(&directory.join(entry)).unwrap();
            cgs.bind_registry_entry_id(entry);
            compiled.insert(
                entry.into(),
                Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap()),
            );
            catalogs.insert(entry.into(), cgs);
        }
        let business = CapabilityRef {
            catalog: "consumer".into(),
            capability: "file_query".into(),
        };
        let deployments = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: business.clone(),
                requirement: "source_session".into(),
                provider_catalog: "source".into(),
                provider: "session".into(),
            }],
        };
        let allowed = ["consumer".into(), "source".into()].into();
        let closure = plasm_core::prerequisites::prerequisite_closure(
            &catalogs
                .iter()
                .map(|(key, cgs)| (key.clone(), cgs))
                .collect(),
            &deployments,
            &[business],
            &allowed,
        )
        .unwrap();
        let pin = DiscoverySessionPin {
            authorization: plasm_agent_core::discovery_store::DiscoveryAuthorization::catalogs(
                allowed,
            ),
            generation: "fixture-generation".into(),
            pin_id: plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string(),
        };
        let mut engine =
            AgentEngine::from_pinned_generation(catalogs, compiled, pin, deployments.clone());
        for _ in 0..2 {
            engine
                .expose_routing("inspect fixture files", &closure)
                .await
                .unwrap();
            assert!(engine.host.is_none());
            let session = engine.ensure_execute_session().unwrap();
            assert_eq!(session.prerequisite_deployments(), &deployments);
        }
    }

    #[tokio::test]
    async fn native_context_waves_publish_before_disclosure_without_opening_corpus() {
        use plasm_core::prerequisites::{CapabilityRef, PrerequisiteClosure};
        let (mut engine, _) = tiny_engine();
        engine.session_id =
            plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string();
        engine
            .configure_workflow_learning(NativeLearningConfig::Collect(
                plasm_agent_core::workflow_learning::native::NativeLibraryConfig {
                    scope: plasm_agent_core::workflow_learning::model::LibraryScope {
                        tenant: "native-wave-fixture".into(),
                        project: "offline".into(),
                    },
                    // Exposure must not attempt a connection to the optional learning store.
                    database_url: "postgresql://127.0.0.1:1/unavailable".into(),
                },
            ))
            .unwrap();
        let route = |name: &str| PrerequisiteClosure {
            business: vec![CapabilityRef {
                catalog: "matrix".into(),
                capability: name.into(),
            }],
            input_sources: vec![],
            prerequisites: vec![],
            acquisitions: vec![],
            edges: vec![],
        };
        engine
            .expose_routing("list", &route("product_list"))
            .await
            .unwrap();
        let stale_session = engine.execute_session.clone();
        let stale_exposure = engine.exposure.clone();
        let stale_teaching = engine.python_teaching.clone();
        let stale_capabilities = engine.capabilities.clone();
        for name in ["category_get", "product_search", "product_list"] {
            engine.expose_routing(name, &route(name)).await.unwrap();
            let host = engine.host.as_ref().unwrap();
            let published = host
                .get_execute_session(AGENT_PROMPT_HASH, engine.execute_session_id.as_str())
                .await
                .unwrap();
            assert_eq!(
                published.domain_revision,
                engine.execute_session.as_ref().unwrap().domain_revision
            );
            assert_eq!(
                published.entities,
                engine.execute_session.as_ref().unwrap().entities
            );
        }
        let authoritative_revision = engine.execute_session.as_ref().unwrap().domain_revision;
        // A genuine stale writer cannot replace the later descriptor or install its local wave.
        engine.execute_session = stale_session;
        engine.exposure = stale_exposure;
        engine.python_teaching = stale_teaching;
        engine.capabilities = stale_capabilities;
        let before_revision = engine.execute_session.as_ref().unwrap().domain_revision;
        let before_intent = engine.intent.clone();
        let before_capabilities = engine.capabilities.clone();
        let logical =
            plasm_agent_core::session_identity::LogicalSessionId::parse(&engine.session_id)
                .unwrap();
        let before_provenance = engine
            .host
            .as_ref()
            .unwrap()
            .logical_sessions
            .get(logical)
            .await
            .unwrap()
            .accumulated_intent;
        let error = engine
            .expose_routing("stale", &route("product_search"))
            .await
            .unwrap_err();
        assert!(matches!(error, AgentEngineError::Workflow(error)
            if matches!(*error, NativeLearningError::Persistence(
                plasm_agent_core::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError::HotBehindDurable {
                    hot_revision, durable_revision,
                }
            ) if hot_revision == before_revision + 1 && durable_revision == authoritative_revision)));
        assert_eq!(
            engine
                .host
                .as_ref()
                .unwrap()
                .logical_sessions
                .get(logical)
                .await
                .unwrap()
                .accumulated_intent,
            before_provenance
        );
        assert_eq!(
            engine.execute_session.as_ref().unwrap().domain_revision,
            before_revision
        );
        assert_eq!(engine.intent, before_intent);
        assert_eq!(engine.capabilities, before_capabilities);
        let published = engine
            .host
            .as_ref()
            .unwrap()
            .get_execute_session(AGENT_PROMPT_HASH, engine.execute_session_id.as_str())
            .await
            .unwrap();
        assert_eq!(published.domain_revision, authoritative_revision);
    }

    #[tokio::test]
    async fn routed_extension_preserves_symbols_and_reviewed_plans() {
        use plasm_core::prerequisites::{CapabilityRef, PrerequisiteClosure};
        let (mut engine, _) = tiny_engine();
        let pin = DiscoverySessionPin {
            authorization: plasm_agent_core::discovery_store::DiscoveryAuthorization::catalogs(
                ["matrix".into()].into(),
            ),
            generation: "generation-one".into(),
            pin_id: engine.session_id.clone(),
        };
        let catalogs = engine
            .catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), cgs.as_ref().clone()))
            .collect();
        let compiled_catalogs = engine.compiled_catalogs.clone().into_iter().collect();
        engine = AgentEngine::from_pinned_generation(
            catalogs,
            compiled_catalogs,
            pin.clone(),
            Default::default(),
        );
        let route = |name: &str| PrerequisiteClosure {
            business: vec![CapabilityRef {
                catalog: "matrix".into(),
                capability: name.into(),
            }],
            input_sources: vec![],
            prerequisites: vec![],
            acquisitions: vec![],
            edges: vec![],
        };
        engine
            .expose_routing("list products", &route("product_list"))
            .await
            .unwrap();
        assert_eq!(
            engine.exposed_capabilities(),
            route("product_list").business
        );
        let initial_entities = engine.exposure.as_ref().unwrap().entities.clone();
        let repeated = engine
            .expose_routing("list again", &route("product_list"))
            .await
            .unwrap();
        assert!(!repeated.declarations_changed);
        assert!(
            !repeated.delta_refs.is_empty(),
            "touched entities are not new declarations"
        );
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
            .await
            .unwrap();
        assert_eq!(
            engine
                .execute_session
                .as_ref()
                .unwrap()
                .discovery_pin
                .as_ref(),
            Some(&pin)
        );
        let extension = engine
            .expose_routing("fetch category", &route("category_get"))
            .await
            .unwrap();
        assert!(extension.declarations_changed);
        assert!(!extension
            .prompt
            .contains(plasm_core::prompt_render::python::LANGUAGE));
        assert!(!extension.prompt.contains("Replace the complete"));
        assert!(
            !extension.prompt.contains("e1.query("),
            "extension must not repeat the delivered operation"
        );
        assert!(engine
            .exposure
            .as_ref()
            .unwrap()
            .entities
            .starts_with(&initial_entities));
        let refs = engine.exposed_capabilities();
        let before_empty = engine.exposure.as_ref().unwrap().entities.clone();
        let empty = PrerequisiteClosure {
            business: vec![],
            input_sources: vec![],
            prerequisites: vec![],
            acquisitions: vec![],
            edges: vec![],
        };
        let unchanged = engine
            .expose_routing("continue using existing capabilities", &empty)
            .await
            .unwrap();
        assert!(unchanged.delta_refs.is_empty());
        assert!(!unchanged.declarations_changed);
        assert_eq!(engine.exposed_capabilities(), refs);
        assert_eq!(engine.exposure.as_ref().unwrap().entities, before_empty);
        assert_eq!(refs.len(), 2);
        assert!(!refs.iter().any(|c| c.capability == "product_search"));
        assert_eq!(engine.discovery_pin(), Some(&pin));
        assert_eq!(
            engine
                .execute_session
                .as_ref()
                .unwrap()
                .discovery_pin
                .as_ref(),
            Some(&pin)
        );
        assert!(engine
            .run_plan(&dry.plan_commit_ref)
            .unwrap()
            .message
            .contains("validated"));
        let same_entity = engine
            .expose_routing("search products", &route("product_search"))
            .await
            .unwrap();
        assert!(
            same_entity.declarations_changed,
            "new method on existing entity is an addition"
        );
        let repeated = engine
            .expose_routing("search again", &route("product_search"))
            .await
            .unwrap();
        assert!(!repeated.declarations_changed);
    }

    #[test]
    fn runtime_loader_rejects_raw_authoring_schema() {
        assert!(AgentEngine::new()
            .load_catalog(&execute_tiny_dir())
            .is_err());
    }

    /// Same acquisition pattern as NAPI `PlasmEngine` (async Mutex, no blocking_lock).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_dry_runs_under_async_mutex_complete() {
        use std::time::{Duration, Instant};
        use tokio::sync::Mutex;

        let (mut boot, info) = tiny_engine();
        boot.expose_seeds(
            "concurrent dry",
            &[CapabilitySeed {
                entry_id: info.entry_id.clone(),
                entity: "Product".into(),
            }],
        )
        .await
        .expect("expose");
        let shared = Arc::new(Mutex::new(boot));

        let started = Instant::now();
        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let eng = Arc::clone(&shared);
            handles.push(tokio::spawn(async move {
                let mut g = eng.lock().await;
                g.dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
                    .await
            }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            let dry = h
                .await
                .expect("join")
                .unwrap_or_else(|e| panic!("dry_run[{i}]: {e}"));
            assert!(
                dry.plan_commit_ref.starts_with("pc"),
                "{}",
                dry.plan_commit_ref
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "parallel dry_run hung ({:?})",
            started.elapsed()
        );
    }
    #[tokio::test]
    async fn native_paging_preserves_origin_and_rows_after_federation_expands() {
        assert_native_paging("e1.query()", false).await;
    }

    #[tokio::test]
    async fn native_complete_computed_rowset_uses_the_same_paging_protocol() {
        assert_native_paging("e1.query().select('id', 'n')", true).await;
    }

    #[tokio::test]
    async fn native_synthetic_projection_pages_preserve_typed_rows() {
        assert_native_paging("e1.query().select('id', n='n')", true).await;
    }

    async fn assert_native_paging(expression: &str, complete_source: bool) {
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        struct Pages(Arc<std::sync::atomic::AtomicUsize>);
        #[async_trait]
        impl HttpTransport for Pages {
            async fn send_compiled_http(
                &self,
                base_url: &str,
                request: &CompiledRequest,
                _: Option<plasm_runtime::auth::ResolvedAuth>,
            ) -> std::result::Result<(serde_json::Value, Option<String>), plasm_runtime::RuntimeError>
            {
                assert_eq!(
                    base_url.trim_end_matches('/'),
                    "https://origin.example",
                    "continuation dispatched to wrong catalog"
                );
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let offset = request
                    .query
                    .as_ref()
                    .and_then(Value::as_object)
                    .and_then(|q| q.get("offset"))
                    .and_then(Value::as_number)
                    .unwrap_or(0.0) as usize;
                let rows: Vec<_> = (offset..(offset + 20).min(88))
                    .map(|n| serde_json::json!({"id":n.to_string(),"n":n}))
                    .collect();
                Ok((serde_json::json!({"results":rows}), None))
            }
            async fn get_json_absolute(
                &self,
                _: &str,
                _: Option<plasm_runtime::auth::ResolvedAuth>,
            ) -> std::result::Result<(serde_json::Value, Option<String>), plasm_runtime::RuntimeError>
            {
                panic!("unexpected absolute GET")
            }
        }
        let mut cgs = plasm_core::load_schema_dir(
            &PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("test manifest directory"))
                .join("../../fixtures/schemas/plasm_pagination_matrix"),
        )
        .unwrap();
        cgs.http_backend = "https://origin.example".into();
        cgs.bind_registry_entry_id("paging");
        let mut other = cgs.clone();
        other.http_backend = "https://other.example".into();
        other.bind_registry_entry_id("other");
        let other_compiled =
            Arc::new(plasm_compile::compile_cgs_capability_templates(&other).unwrap());
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        let mut engine = AgentEngine::from_generation(
            [("paging".into(), cgs), ("other".into(), other)].into(),
            [
                ("paging".into(), compiled),
                ("other".into(), other_compiled),
            ]
            .into(),
            "paging-session".into(),
        );
        engine
            .expose_seeds(
                "list rows",
                &[CapabilitySeed {
                    entry_id: "paging".into(),
                    entity: "ItemOffset".into(),
                }],
            )
            .await
            .unwrap();
        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dry = engine
            .dry_run(&format!(
                "class Read(Program):\n    def build(self):\n        return {expression}\n"
            ))
            .await
            .unwrap();
        let first = engine
            .run_plan_live(&dry.plan_commit_ref, Arc::new(Pages(dispatches.clone())))
            .await
            .unwrap();
        assert!(first.ok, "{} {:?}", first.message, first.failure_json);
        let meta: serde_json::Value =
            serde_json::from_str(first.meta_json.as_deref().unwrap()).unwrap();
        assert!(meta["plasm"]["paging"][0]["next_run_ref"].is_string());
        engine
            .expose_seeds(
                "expand to another catalog",
                &[CapabilitySeed {
                    entry_id: "other".into(),
                    entity: "ItemOffset".into(),
                }],
            )
            .await
            .unwrap();
        let initial_dispatches = dispatches.load(std::sync::atomic::Ordering::SeqCst);
        let mut current = first;
        let mut rows = Vec::new();
        let mut completed = false;
        for _ in 0..10 {
            let page: serde_json::Value =
                serde_json::from_str(current.rows_json.as_deref().unwrap()).unwrap();
            rows.extend(page["rows"].as_array().unwrap().iter().cloned());
            let meta: serde_json::Value =
                serde_json::from_str(current.meta_json.as_deref().unwrap()).unwrap();
            let next = meta["plasm"]["paging"][0]["next_run_ref"].as_str();
            if next.is_none() {
                assert_eq!(page["coverage"], "complete");
                completed = true;
                break;
            }
            assert_eq!(
                page["coverage"],
                if complete_source {
                    "complete"
                } else {
                    "partial"
                }
            );
            let next = next.expect("every undelivered page must publish its continuation");
            current = engine
                .run_plan_live(next, Arc::new(Pages(dispatches.clone())))
                .await
                .unwrap();
        }
        if complete_source {
            assert_eq!(
                dispatches.load(std::sync::atomic::Ordering::SeqCst),
                initial_dispatches,
                "delivery continuation must not re-execute acquisition"
            );
        }
        assert!(
            completed,
            "paging must terminate within the fixture's bounded page count"
        );
        assert_eq!(rows.len(), 88);
        let ids: std::collections::BTreeSet<_> =
            rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids.len(),
            88,
            "no dropped or repeated rows across native paging"
        );
    }
}

#[cfg(test)]
#[path = "hydration_boundary_tests.rs"]
mod hydration_boundary_tests;
