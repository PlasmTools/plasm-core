//! In-process agent engine: catalog load, agent-global symbol exposure, dry-run.

use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use plasm_agent_core::discovery_store::DiscoverySessionPin;
use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::CapabilitySeed;
use plasm_agent_core::operation::{
    compute_plan_commit_id_from_dry, PlanCommitRecord, PLAN_COMMIT_TTL,
};
use plasm_agent_core::plan_commit_store::{dry_for_committed_plasm_run, resolve_committed_plan};
use plasm_agent_core::plasm_compile::compile_plasm_expression;
use plasm_agent_core::plasm_plan_run::run_plasm_comp;
use plasm_agent_core::plasm_plan_run::{
    evaluate_plasm_comp_dry, plan_dry_compact_view, render_plasm_plan_dry_text_for_session,
};
use plasm_agent_core::program_diagnostic::{ProgramDiagnostic, ProgramStageError};
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_agent_core::PlasmCompBundle;
use plasm_core::discovery::{CgsRegistry, RegistryEntryPair};
use plasm_core::prompt_render::{teaching_tsv_from_wrapped_prompt, TeachingFenceSlice};
use plasm_core::PlanCommitRef;
use plasm_core::{
    capability_method_label_kebab, ExposureEntityKey, InputSchema, NamedValueSchema, OutputSchema,
    PromptPipelineConfig, SymbolMapCrossRequestCache, TeachingExposureSession, CGS,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Stable execute-session wire ids for in-process agent runs (run artifact store keys).
const AGENT_PROMPT_HASH: &str = "plasm_node";

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
pub struct CapabilityIntrospection {
    pub name: String,
    pub kind: String,
    pub entity: String,
    pub invoke_wire_name: String,
    pub input_schema: Option<InputSchema>,
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
    pub tsv: String,
    pub delta_refs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DryRunResult {
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
    session_id: String,
    discovery_pin: Option<DiscoverySessionPin>,
    intent: String,
    manifest_paths: Vec<std::path::PathBuf>,
    capabilities: Vec<(String, String)>,
    catalogs: IndexMap<String, Arc<CGS>>,
    compiled_catalogs: IndexMap<String, Arc<plasm_compile::CompiledCatalog>>,
    catalog_digests: IndexMap<String, String>,
    exposure: Option<TeachingExposureSession>,
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
            pipeline: PromptPipelineConfig::default(),
            sym_cross: SymbolMapCrossRequestCache::from_env(),
            execute_session: None,
        }
    }

    pub fn set_intent(&mut self, intent: impl Into<String>) {
        self.intent = intent.into();
    }

    pub fn introspect_catalog(&self, entry_id: &str) -> Result<CatalogIntrospection> {
        let cgs = self
            .catalogs
            .get(entry_id)
            .ok_or_else(|| anyhow!("catalog `{entry_id}` not loaded — call loadCatalog first"))?;
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
                // Prefer payload, else arguments (CapabilityInputs lanes — no legacy `input_schema`).
                input_schema: cap
                    .inputs
                    .payload
                    .clone()
                    .or_else(|| cap.inputs.arguments.clone()),
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
            return Err(anyhow!(
                "loadCatalog requires a format-3 packed manifest; repack raw YAML first"
            ));
        }
        let manifest = plasm_core::catalog_il::read_catalog_manifest(catalog_path)
            .map_err(anyhow::Error::msg)?;
        let cgs = plasm_core::catalog_il::load_catalog_artifact(
            catalog_path
                .parent()
                .ok_or_else(|| anyhow!("manifest directory missing"))?,
            &manifest,
        )
        .map_err(anyhow::Error::msg)?;
        let compiled = plasm_compile::load_compiled_catalog_artifact(
            catalog_path
                .parent()
                .ok_or_else(|| anyhow!("manifest directory missing"))?,
            &manifest,
            &cgs,
        )
        .map_err(anyhow::Error::msg)?;
        let entry_id = manifest.entry_id;
        let digest = cgs.catalog_cgs_hash_hex();
        if let Some(existing) = self.catalog_digests.get(&entry_id) {
            if existing != &digest {
                return Err(anyhow!(
                    "catalog digest changed for `{entry_id}` — start a fresh agent symbol space"
                ));
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
                .ok_or_else(|| anyhow!("missing pinned catalog {entry}"))?
                .clone();
            let delta = plasm_core::capability_exposure::selected_capability_surface(
                &cgs,
                &entry,
                &capabilities,
            )
            .map_err(anyhow::Error::msg)?;
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
            .ok_or_else(|| anyhow!("ready routing has no teaching exposure"))?;
        if let Some(session) = &mut self.execute_session {
            // Keep graph state and reviewed plans while appending symbols.
            session.entities = exposure.entities.clone();
            session.teaching_exposure = Some(exposure.clone());
            session.context_intent = Some(self.intent.clone());
            session.domain_revision += 1;
        }
        let mut tsv = self.render_teaching_delta(&touched)?;
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
        )
        .map_err(anyhow::Error::msg)?;
        if !guidance.is_empty() {
            tsv.push_str("\n\n");
            tsv.push_str(&guidance);
        }
        Ok(TeachingExposureResult {
            tsv,
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
            let cgs = self
                .catalogs
                .get(&seed.entry_id)
                .ok_or_else(|| anyhow!("unknown catalog {}", seed.entry_id))?;
            if !cgs.entities.contains_key(seed.entity.as_str()) {
                return Err(anyhow!("unknown entity {}", seed.entity));
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
    ) -> anyhow::Error {
        let diag = ProgramDiagnostic::from_stage(
            &self.pipeline,
            Some(&self.sym_cross),
            es,
            program,
            stage,
        );
        anyhow!("{}", diag.agent_markdown())
    }

    pub fn dry_run(&mut self, program: &str) -> Result<DryRunResult> {
        let trimmed = program.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("program is empty"));
        }
        let es = self.ensure_execute_session()?;
        let bundle = match compile_plasm_expression(
            &self.pipeline,
            Some(&self.sym_cross),
            &es,
            "plasm_node",
            trimmed,
        ) {
            Ok(b) => b,
            Err(stage) => return Err(self.reject_from_stage(&es, trimmed, stage)),
        };
        let dry = match evaluate_plasm_comp_dry(&es, &bundle) {
            Ok(d) => d,
            Err(stage) => return Err(self.reject_from_stage(&es, trimmed, stage)),
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
        .map_err(|denial| {
            anyhow!(
                "plan commit blocked by flow policy ({:?}): {} violation(s)",
                denial.verdict,
                denial.violations.len()
            )
        })?;
        es.register_plan_commit(record);
        self.execute_session = Some(es);
        Ok(DryRunResult {
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
            return Err(anyhow!("missing `plan_commit_ref`"));
        }
        let commit_ref = PlanCommitRef::parse(trimmed)
            .ok_or_else(|| anyhow!("invalid plan_commit_ref `{trimmed}`"))?;
        let es = self.ensure_execute_session()?;
        resolve_committed_plan(&es, &commit_ref).map_err(|e| {
            anyhow!(
                "unknown or expired plan_commit_ref `{trimmed}` — call `plasm` (dry-run) first: {e:?}"
            )
        })?;
        Ok(RunPlanResult {
            ok: false,
            message: format!("Plan `{trimmed}` validated. Pass a HostTransportFn to execute live."),
            rows_json: None,
            meta_json: None,
            artifacts_json: None,
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
            return Err(anyhow!("missing `plan_commit_ref`"));
        }
        let commit_ref = PlanCommitRef::parse(trimmed)
            .ok_or_else(|| anyhow!("invalid plan_commit_ref `{trimmed}`"))?;
        let es = self.ensure_execute_session()?;
        let committed = resolve_committed_plan(&es, &commit_ref).map_err(|e| {
            anyhow!(
                "unknown or expired plan_commit_ref `{trimmed}` — call `plasm` (dry-run) first: {e:?}"
            )
        })?;
        let bundle = PlasmCompBundle::new(committed.artifact.clone())
            .map_err(|e| anyhow!("invalid committed plan artifact: {e}"))?;
        let dry = dry_for_committed_plasm_run(&es, &bundle, &committed)
            .map_err(|e| anyhow!("dry evaluation for committed plan: {e}"))?;

        let host = self.build_host_state(transport)?;
        let live = Box::pin(run_plasm_comp(
            &es,
            &host,
            AGENT_PROMPT_HASH,
            &self.session_id,
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await
        .map_err(|e| anyhow!("live execute failed: {e}"))?;

        let message = live
            .run_markdown
            .clone()
            .unwrap_or_else(|| format!("Live run completed for `{trimmed}`."));
        let mut artifacts = Vec::new();
        for artifact in &live.code_plan_run_artifacts {
            let id = plasm_agent_core::run_artifacts::RunArtifactId::from_wire(&artifact.run_id)
                .ok_or_else(|| anyhow!("invalid canonical run id"))?;
            let payload = host
                .run_artifacts
                .get_payload_result(AGENT_PROMPT_HASH, &self.session_id, id)
                .await
                .map_err(|error| anyhow!("reading canonical run artifact: {error}"))?
                .ok_or_else(|| anyhow!("canonical run artifact missing"))?;
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
            artifacts_json: Some(serde_json::to_string(&artifacts)?),
        })
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
            return Err(anyhow!("no catalogs loaded — call loadCatalog first"));
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
        Ok(build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(registry),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        }))
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

    fn by_entry_cgs(&self) -> IndexMap<String, &CGS> {
        self.catalogs
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref()))
            .collect()
    }

    fn render_teaching_delta(&self, all_new_qualified: &[ExposureEntityKey]) -> Result<String> {
        let exp = self
            .exposure
            .as_ref()
            .ok_or_else(|| anyhow!("exposure missing for render"))?;
        let by_entry = self.by_entry_cgs();
        let rendered = if by_entry.len() <= 1 {
            let (_entry_id, cgs) = by_entry
                .iter()
                .next()
                .ok_or_else(|| anyhow!("no catalogs loaded"))?;
            let added_refs: Vec<&str> = all_new_qualified
                .iter()
                .map(|k| k.entity.as_str())
                .collect();
            self.pipeline.render_teaching_exposure_delta(
                cgs,
                exp,
                &added_refs,
                Some(&self.sym_cross),
            )
        } else {
            self.pipeline.render_teaching_exposure_delta_federated(
                &by_entry,
                exp,
                all_new_qualified,
                Some(&self.sym_cross),
            )
        };
        let mode = self.pipeline.render_mode;
        Ok(
            plasm_core::teaching_tsv_table_from_wrapped_prompt_any(&rendered)
                .or_else(|| {
                    teaching_tsv_from_wrapped_prompt(
                        &rendered,
                        mode.markdown_fence_info_string(),
                        TeachingFenceSlice::TableOnly,
                    )
                })
                .unwrap_or(rendered),
        )
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
            primary_entry_id_from_seeds(&seeds).ok_or_else(|| anyhow!("no catalogs loaded"))?;
        if !self.catalogs.contains_key(&primary_api) {
            return Err(anyhow!("missing primary catalog `{primary_api}`"));
        }
        let cgs = self
            .catalogs
            .get(&primary_api)
            .ok_or_else(|| anyhow!("missing primary catalog `{primary_api}`"))?
            .clone();
        let exposure = self
            .exposure
            .clone()
            .ok_or_else(|| anyhow!("no symbol exposure — call exposeSeeds first"))?;
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
        ResultCoverage,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

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
                count: 1,
                entities: vec![entity],
                has_more: false,
                coverage: ResultCoverage::Unknown,
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
    fn load_expose_and_dry_run_execute_tiny() {
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
        assert!(teaching.tsv.contains("e1") || !teaching.tsv.is_empty());
        let dry = engine.dry_run("e1").expect("dry run");
        assert!(dry.plan_commit_ref.starts_with("pc"));
        assert!(!dry.summary.is_empty());
        assert!(
            !dry.fused_clean_read,
            "unbounded Product list must stay on run_ref: {}",
            dry.summary
        );
    }

    #[test]
    fn identical_dry_run_reject_names_already_rejected() {
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
        let program = "rows = e1\nbad = rows | select not_a_taught_field\nbad";
        let first = engine
            .dry_run(program)
            .expect_err("unknown field is a compile reject")
            .to_string();
        assert!(
            first.contains("not a row field"),
            "first reject names the field: {first}"
        );
        assert!(
            !first.contains("already rejected"),
            "first reject is fresh: {first}"
        );
        let second = engine
            .dry_run(program)
            .expect_err("identical resubmit is a compile reject")
            .to_string();
        assert!(
            second.contains("This exact program was already rejected"),
            "NAPI dry_run must record through from_stage: {second}"
        );
    }

    #[test]
    fn identical_parse_reject_names_already_rejected() {
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
            .expect_err("extra identity args are a parse reject")
            .to_string();
        assert!(
            !first.contains("already rejected"),
            "first parse reject is fresh: {first}"
        );
        let second = engine
            .dry_run(program)
            .expect_err("identical parse resubmit is a reject")
            .to_string();
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
        let notice_full = "[notice_id,author_email,body,created_at,title]";
        let notice_summary = "[notice_id,title]";
        let tx_full = "[transaction_id,amount,created_at,description,private]";
        let tx_summary = "[transaction_id,amount,description]";
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
        assert!(
            teaching.tsv.contains(notice_full),
            "exposeSeeds must teach RA-12 {notice_full}; card:\n{}",
            teaching.tsv
        );
        assert!(
            !teaching.tsv.contains(notice_summary),
            "exposeSeeds must not teach provides {notice_summary}; card:\n{}",
            teaching.tsv
        );
        assert!(
            teaching.tsv.contains(tx_full),
            "exposeSeeds must teach RA-12 {tx_full}; card:\n{}",
            teaching.tsv
        );
        assert!(
            !teaching.tsv.contains(tx_summary),
            "exposeSeeds must not teach T132207 provides {tx_summary}; card:\n{}",
            teaching.tsv
        );
        assert!(
            teaching.tsv.contains("author_email")
                && teaching.tsv.contains("created_at")
                && teaching.tsv.contains("private"),
            "sheared decode fields must appear on the exposeSeeds card:\n{}",
            teaching.tsv
        );
    }

    #[test]
    fn live_run_serialization_populates_rows_and_meta() {
        let live = synthetic_live_run_result();
        let rows = live_run_rows_json(&live).expect("rows_json");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&rows).expect("parse rows");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], "p1");
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
            Err(plasm_runtime::error::RuntimeError::ConfigurationError {
                message: "not used".into(),
            })
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
        let dry = engine.dry_run("e1").expect("dry run");
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
        let arr = parsed.as_array().expect("entity rows array");
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
                    let dry = engine.dry_run("items = LangItem\ndone = items => _.update(title=\"checked\", score=2, owner=\"alice\")\ndone").unwrap();
                    let transport = Arc::new(RecordingItemTransport(std::sync::Mutex::new(Vec::new())));
                    let live = engine.run_plan_live(&dry.plan_commit_ref, transport.clone()).await.unwrap();
                    assert!(live.ok, "{}", live.message);
                    assert_eq!(*transport.0.lock().unwrap(), ["/language/v1/items/p1", "/language/v1/items/p2"]);
                });
        }).unwrap().join().unwrap();
    }

    #[test]
    fn routed_extension_preserves_symbols_and_reviewed_plans() {
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
        let dry = engine.dry_run("e1").unwrap();
        assert_eq!(
            engine
                .execute_session
                .as_ref()
                .unwrap()
                .discovery_pin
                .as_ref(),
            Some(&pin)
        );
        engine
            .expose_routing("fetch category", &route("category_get"))
            .unwrap();
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
                g.dry_run("e1")
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
}
