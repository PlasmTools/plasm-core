//! In-process agent engine: catalog load, agent-global symbol exposure, dry-run.

use indexmap::IndexMap;
use plasm_agent_core::discovery_store::DiscoverySessionPin;
use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
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
const AGENT_PROMPT_HASH: &str = "plasm_node";

type Result<T> = std::result::Result<T, AgentEngineError>;

#[derive(Debug, Error)]
pub enum AgentEngineError {
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
            message: failure.diagnostic().to_owned(),
            rows_json: None,
            meta_json: None,
            artifacts_json: None,
            failure_json: Some(serde_json::to_string(&failure).expect("typed failure serializes")),
        }
    }
}

fn live_run_rows_json(
    live: &plasm_agent_core::plasm_plan_run::PlasmPlanRunResult,
) -> Option<String> {
    use plasm_agent_core::output::http_execute_results_value;
    let steps = &live.return_steps;
    if steps.is_empty() {
        return None;
    }
    let value = if steps.len() == 1 {
        http_execute_results_value(&steps[0].result)
    } else {
        serde_json::Value::Array(
            steps
                .iter()
                .map(|s| http_execute_results_value(&s.result))
                .collect(),
        )
    };
    serde_json::to_string(&value).ok()
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
    discovery_pin: Option<DiscoverySessionPin>,
    intent: String,
    manifest_paths: Vec<std::path::PathBuf>,
    capabilities: Vec<(String, String)>,
    catalogs: IndexMap<String, Arc<CGS>>,
    compiled_catalogs: IndexMap<String, Arc<plasm_compile::CompiledCatalog>>,
    catalog_digests: IndexMap<String, String>,
    exposure: Option<TeachingExposureSession>,
    python_teaching: plasm_core::prompt_render::python::PythonTeachingState,
    pipeline: PromptPipelineConfig,
    sym_cross: SymbolMapCrossRequestCache,
    /// Cached execute session with registered plan commits (invalidated on exposure change).
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
            discovery_pin: None,
            intent: String::new(),
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
    }

    pub(crate) fn set_python_pool(&mut self, pool: Arc<plasm_agent_core::python_pool::PythonPool>) {
        self.python_pool = pool;
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
    ) -> Self {
        let mut engine = Self::from_generation(catalogs, compiled_catalogs, pin.pin_id.clone());
        engine.discovery_pin = Some(pin);
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

    pub fn expose_routing(
        &mut self,
        intent: &str,
        closure: &plasm_core::prerequisites::PrerequisiteClosure,
    ) -> Result<TeachingExposureResult> {
        if !self.intent.is_empty() {
            self.intent.push('\n');
        }
        self.intent.push_str(intent);
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
        for (entry, capabilities) in grouped {
            let cgs = self
                .catalogs
                .get(&entry)
                .ok_or_else(|| AgentEngineError::PinnedCatalogMissing {
                    entry_id: entry.clone(),
                })?
                .clone();
            let delta = plasm_core::capability_exposure::selected_capability_surface(
                &cgs,
                &entry,
                &capabilities,
            )?;
            let entities: Vec<_> = delta
                .required
                .entities
                .iter()
                .map(|e| e.entity.to_string())
                .collect();
            for entity in &entities {
                if !self.has_capability(&entry, entity) {
                    self.capabilities.push((entry.clone(), entity.clone()));
                }
            }
            touched.extend(delta.required.entities.iter().cloned());
            let refs = entities.iter().map(String::as_str).collect::<Vec<_>>();
            if let Some(exposure) = &mut self.exposure {
                let layers = self.catalogs.values().map(Arc::as_ref).collect::<Vec<_>>();
                exposure.expose_surface(&layers, cgs, &entry, &refs, delta);
            } else {
                self.exposure = Some(TeachingExposureSession::new_with_intent_delta(
                    &cgs, &entry, &refs, delta,
                ));
            }
        }
        let exposure = self
            .exposure
            .as_ref()
            .cloned()
            .ok_or(AgentEngineError::RoutingExposureMissing)?;
        if let Some(session) = &mut self.execute_session {
            // Keep graph state and reviewed plans while appending symbols.
            session.entities = exposure.entities.clone();
            session.teaching_exposure = Some(exposure.clone());
            session.context_intent = Some(self.intent.clone());
            session.domain_revision += 1;
        }
        let mut prompt = self.render_teaching_delta(&touched)?;
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
        Ok(TeachingExposureResult {
            prompt,
            delta_refs: touched
                .iter()
                .map(|e| format!("{}:{}", e.entry_id, e.entity))
                .collect(),
        })
    }

    /// Explicit local execution may expose a caller-named entity without discovery.
    /// Every capability is selected by exact ownership; intent never controls admission.
    pub fn expose_seeds(
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
                return Ok(self.reject_from_stage(&es, trimmed, *stage))
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
                    summary: failure.to_string(),
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
        let trimmed = plan_commit_ref.trim();
        if trimmed.is_empty() {
            return Ok(RunPlanResult::reference_rejection(
                "invalid_run_reference",
                "Copy a reviewed commit reference or paging handle verbatim.",
            ));
        }
        let es = self.ensure_execute_session()?;
        let (bundle, dry) = match ExecutableRunTarget::parse(trimmed) {
            Ok(ExecutableRunTarget::Commit(commit_ref)) => {
                let committed = match resolve_committed_plan(&es, &commit_ref) {
                    Ok(committed) => committed,
                    Err(error) => return Ok(RunPlanResult::failure(error.into())),
                };
                let bundle = PlasmCompBundle::new(committed.artifact.clone())
                    .map_err(AgentEngineError::CommittedPlanArtifact)?;
                let dry = match dry_for_committed_plasm_run(&es, &bundle, &committed) {
                    Ok(dry) => dry,
                    Err(error) => return Ok(RunPlanResult::failure(error.into())),
                };
                (bundle, dry)
            }
            Ok(ExecutableRunTarget::Page(handle)) => {
                let bundle = match plasm_agent_core::mcp_server::compile_page_continuation(
                    &es, &handle, 0,
                ) {
                    Ok(bundle) => bundle,
                    Err(failure) => return Ok(RunPlanResult::failure(failure)),
                };
                let dry = evaluate_plasm_comp_dry(&es, &bundle).map_err(AgentEngineError::from)?;
                (bundle, dry)
            }
            Err(_) => {
                return Ok(RunPlanResult::reference_rejection(
                "invalid_run_reference",
                format!("Run reference `{trimmed}` is not executable. Copy a reviewed commit reference from plasm or a continuation handle from plasm_run verbatim; run artifact IDs identify observations, not executable plans."),
            ));
            }
        };

        let host = self.build_host_state(transport)?;
        let live_result = Box::pin(run_plasm_comp(
            &es,
            &host,
            AGENT_PROMPT_HASH,
            &self.session_id,
            &bundle,
            true,
            None,
            None,
            Some(dry),
            Some(plasm_agent_core::McpResultTransportPolicy {
                artifact_access: plasm_agent_core::ArtifactAccessMode::DagCompute,
                ..Default::default()
            }),
        ))
        .await;
        let live = match live_result {
            Ok(live) => live,
            Err(failure) => {
                return Ok(RunPlanResult {
                    ok: false,
                    message: failure.to_string(),
                    rows_json: None,
                    meta_json: None,
                    artifacts_json: None,
                    failure_json: Some(serde_json::to_string(&failure)?),
                })
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
                    .get_payload_result(AGENT_PROMPT_HASH, &self.session_id, id)
                    .await
                    .map_err(AgentEngineError::RunArtifact)?
                    .ok_or(AgentEngineError::RunArtifactMissing)?;
                artifacts.push(serde_json::json!({
                    "run_id": artifact.run_id,
                    "snapshot": serde_json::from_slice::<serde_json::Value>(&payload.bytes)?,
                }));
            }
            Ok(RunPlanResult {
                ok: true,
                message,
                rows_json: live_run_rows_json(&live),
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
                    message: failure.to_string(),
                    rows_json: None,
                    meta_json: None,
                    artifacts_json: None,
                    failure_json: Some(serde_json::to_string(&failure)?),
                })
            }
        }
    }

    fn build_host_state(
        &self,
        transport: Arc<dyn HttpTransport>,
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
        let engine = ExecutionEngine::new_with_transport(config, transport, None);
        let mut state = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(registry),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })?;
        state.oss.python_pool = self.python_pool.clone();
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

    fn has_capability(&self, api: &str, entity: &str) -> bool {
        self.capabilities
            .iter()
            .any(|(a, e)| a == api && e == entity)
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
        let text = if wave.language.is_none() && wave.declarations.is_empty() {
            String::new()
        } else {
            format!(
                "{}\n{}",
                wave.language.unwrap_or_default(),
                wave.declarations
            )
        };
        self.python_teaching = wave.next_state;
        Ok(text)
    }

    fn build_execute_session(&self) -> Result<ExecuteSession> {
        use plasm_core::CgsContext;

        let mut contexts_by_entry: IndexMap<String, Arc<CgsContext>> = IndexMap::new();
        for (api, cgs) in &self.catalogs {
            contexts_by_entry.insert(api.clone(), Arc::new(CgsContext::entry(api, cgs.clone())));
        }
        let seeds: Vec<CapabilitySeed> = self
            .capabilities
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
        let exposure = self
            .exposure
            .clone()
            .ok_or(AgentEngineError::TeachingExposureMissing)?;
        let entities = exposure.entities.clone();
        let catalog_cgs_hash = cgs.catalog_cgs_hash_hex();
        let mut session = ExecuteSession::new_with_bindings(
            AGENT_PROMPT_HASH.into(),
            self.session_id.clone(),
            cgs,
            contexts_by_entry,
            primary_api,
            String::new(),
            String::new(),
            None,
            entities,
            Some(exposure),
            None,
            catalog_cgs_hash,
            if self.intent.is_empty() {
                None
            } else {
                Some(self.intent.clone())
            },
            IndexMap::new(),
            self.compiled_catalogs.clone(),
        );
        session.discovery_pin = self.discovery_pin.clone();
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

    #[test]
    fn run_reference_admission_is_effect_free_and_correctable() {
        let (mut engine, info) = tiny_engine();
        engine
            .expose_seeds(
                "abstract reference admission",
                &[CapabilitySeed {
                    entry_id: info.entry_id,
                    entity: "Product".into(),
                }],
            )
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plasm-agent-core/tests/fixtures/execute_tiny")
    }

    fn synthetic_live_run_result() -> PlasmPlanRunResult {
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("p1".into()));
        fields.insert("name".into(), Value::String("Widget".into()));
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

    fn return_projection_engine() -> (AgentEngine, CatalogInfo) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

    #[test]
    fn introspection_bindings_match_full_python_exposure() {
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
        let teaching = engine.expose_seeds("", &seeds).unwrap();
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
            .expect("expose");
        assert!(teaching.prompt.contains("e1") || !teaching.prompt.is_empty());
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
            .await
            .expect("dry run");
        assert!(dry.plan_commit_ref.starts_with("pc"));
        assert!(!dry.summary.is_empty());
        assert!(
            !dry.fused_clean_read,
            "unbounded Product list must stay on run_ref: {}",
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
    #[test]
    fn expose_seeds_teaches_ra12_return_projection_not_provides_subset() {
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
        let rows = live_run_rows_json(&live).expect("rows_json");
        let parsed: serde_json::Value = serde_json::from_str(&rows).expect("parse result envelope");
        assert_eq!(parsed["rows"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["rows"][0]["id"], "p1");
        let meta = live_run_meta_json(&live).expect("meta_json");
        assert!(meta.contains("plasm"));
        assert!(meta.contains("abc123"));
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
            .expect("expose");
        let dry = engine
            .dry_run("class Read(Program):\n    def build(self):\n        return e1.query()\n")
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
        }
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
                    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
                    }]).unwrap();
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
        engine = AgentEngine::from_pinned_generation(catalogs, compiled_catalogs, pin.clone());
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
            .unwrap();
        assert_eq!(
            engine.exposed_capabilities(),
            route("product_list").business
        );
        let initial_entities = engine.exposure.as_ref().unwrap().entities.clone();
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
            .unwrap();
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
            .unwrap();
        assert!(unchanged.delta_refs.is_empty());
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
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
