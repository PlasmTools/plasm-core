//! In-process agent engine: catalog load, agent-global symbol exposure, dry-run.

use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use plasm_agent_core::discovery_human_format::{
    format_discovery_markdown_for_mcp, DiscoveryTablePolicy,
};
use plasm_agent_core::execute_session::ExecuteSession;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::CapabilitySeed;
use plasm_agent_core::operation::{
    compute_plan_commit_id_from_dry, PlanCommitDryCache, PlanCommitRecord, PLAN_COMMIT_TTL,
};
use plasm_agent_core::plan_commit_store::{dry_for_committed_plasm_run, resolve_committed_plan};
use plasm_agent_core::plasm_compile::compile_plasm_expression;
use plasm_agent_core::plasm_plan_run::run_plasm_comp;
use plasm_agent_core::plasm_plan_run::{
    evaluate_plasm_comp_dry, plan_dry_compact_view, render_plasm_plan_dry_text_for_session,
};
use plasm_agent_core::run_artifacts::RunArtifactStore;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_agent_core::PlasmCompBundle;
use plasm_core::discovery::{
    derive_intent_exposure_surface_batch, CapabilityQuery, ExposureSurfaceOptions,
    InMemoryCgsRegistry, RegistryEntryPair,
};
use plasm_core::prompt_render::{teaching_tsv_from_wrapped_prompt, TeachingFenceSlice};
use plasm_core::relation_endpoint_keys;
use plasm_core::CgsDiscovery;
use plasm_core::PlanCommitRef;
use plasm_core::{
    capability_method_label_kebab, load_schema, load_schema_dir, ExposureEntityKey, InputSchema,
    NamedValueSchema, OutputSchema, PromptPipelineConfig, SymbolMapCrossRequestCache,
    TeachingExposureSession, CGS,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Stable execute-session wire ids for in-process agent runs (run artifact store keys).
const AGENT_PROMPT_HASH: &str = "plasm_node";
const AGENT_SESSION_ID: &str = "agent";

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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoverResult {
    pub markdown: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunPlanResult {
    pub ok: bool,
    pub message: String,
    pub rows_json: Option<String>,
    pub meta_json: Option<String>,
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
    intent: String,
    capabilities: Vec<(String, String)>,
    catalogs: IndexMap<String, Arc<CGS>>,
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
            intent: String::new(),
            capabilities: Vec::new(),
            catalogs: IndexMap::new(),
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
                input_schema: cap.input_schema.clone(),
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
        let cgs = if catalog_path.is_file() {
            load_schema(catalog_path).map_err(|e| anyhow!("{e}"))?
        } else {
            load_schema_dir(catalog_path).map_err(|e| anyhow!("{e}"))?
        };
        let digest = cgs.catalog_cgs_hash_hex();
        let entry_id = cgs
            .entry_id
            .clone()
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| {
                catalog_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .map(|s| s.strip_suffix(".cgs").unwrap_or(s))
                    .or_else(|| catalog_path.file_name().and_then(|n| n.to_str()))
                    .unwrap_or("default")
                    .to_string()
            });
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
        self.catalog_digests
            .insert(entry_id.clone(), digest.clone());
        self.catalogs.insert(entry_id.clone(), Arc::new(cgs));
        self.invalidate_execute_session();
        Ok(CatalogInfo {
            entry_id,
            catalog_cgs_hash: digest,
        })
    }

    pub fn expose_seeds(
        &mut self,
        intent: impl Into<String>,
        seeds: &[CapabilitySeed],
    ) -> Result<TeachingExposureResult> {
        self.intent = intent.into();
        let mut newly_added: Vec<CapabilitySeed> = Vec::new();
        for s in seeds {
            if !self.has_capability(&s.entry_id, &s.entity) {
                newly_added.push(s.clone());
                self.capabilities
                    .push((s.entry_id.clone(), s.entity.clone()));
            }
        }
        if newly_added.is_empty() {
            return Ok(TeachingExposureResult {
                tsv: String::new(),
                delta_refs: Vec::new(),
            });
        }
        self.invalidate_execute_session();

        let mut grouped: IndexMap<String, Vec<String>> = IndexMap::new();
        for s in &newly_added {
            grouped
                .entry(s.entry_id.clone())
                .or_default()
                .push(s.entity.clone());
        }
        for entities in grouped.values_mut() {
            entities.sort_unstable();
            entities.dedup();
        }

        let mut all_new_qualified: Vec<ExposureEntityKey> = Vec::new();
        let intent_s = self.intent.clone();
        let use_intent = !intent_s.trim().is_empty();
        let intent_ref = intent_s.trim();

        let mut process_order: Vec<String> = grouped.keys().cloned().collect();
        process_order.sort();

        for entry_id in &process_order {
            let Some(entities) = grouped.get(entry_id) else {
                continue;
            };
            let cgs = self
                .catalogs
                .get(entry_id)
                .ok_or_else(|| anyhow!("missing catalog `{entry_id}` — call loadCatalog first"))?;
            let refs: Vec<&str> = entities.iter().map(|s| s.as_str()).collect();
            let n0 = self
                .exposure
                .as_ref()
                .map(|e| e.entities.len())
                .unwrap_or(0);

            self.apply_exposure_wave(
                cgs.clone(),
                entry_id,
                &refs,
                entities,
                use_intent,
                intent_ref,
            )?;

            let exp = self
                .exposure
                .as_ref()
                .ok_or_else(|| anyhow!("exposure missing after expose"))?;
            all_new_qualified.extend(exp.qualified_entities_since(n0));
        }

        if all_new_qualified.is_empty() {
            return Ok(TeachingExposureResult {
                tsv: String::new(),
                delta_refs: Vec::new(),
            });
        }

        let tsv = self.render_teaching_delta(&all_new_qualified)?;
        let delta_refs: Vec<String> = all_new_qualified
            .iter()
            .map(|k| format!("{}:{}", k.entry_id, k.entity))
            .collect();
        Ok(TeachingExposureResult { tsv, delta_refs })
    }

    pub fn discover(&self, intent: &str) -> Result<DiscoverResult> {
        let intent = intent.trim();
        if intent.is_empty() {
            return Err(anyhow!(
                "discover_capabilities `intent` must be a non-empty string"
            ));
        }
        let pairs: Vec<RegistryEntryPair> = self
            .catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), id.clone(), Vec::new(), cgs.clone()))
            .collect();
        if pairs.is_empty() {
            return Err(anyhow!("no catalogs loaded — call loadCatalog first"));
        }
        let registry = InMemoryCgsRegistry::from_pairs(pairs);
        let query = CapabilityQuery {
            tokens: vec![intent.to_string()],
            ..CapabilityQuery::default()
        };
        let result = registry.discover(&query).map_err(|e| anyhow!("{e}"))?;
        let formatted =
            format_discovery_markdown_for_mcp(&result, &DiscoveryTablePolicy::default());
        Ok(DiscoverResult {
            markdown: formatted.markdown,
        })
    }

    pub fn dry_run(&mut self, program: &str) -> Result<DryRunResult> {
        let trimmed = program.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("program is empty"));
        }
        let es = self.ensure_execute_session()?;
        let bundle = compile_plasm_expression(
            &self.pipeline,
            Some(&self.sym_cross),
            &es,
            "plasm_node",
            trimmed,
        )
        .map_err(|e| anyhow!("{e}"))?;
        let dry = evaluate_plasm_comp_dry(&es, &bundle).map_err(|e| anyhow!("{e}"))?;
        let summary = render_plasm_plan_dry_text_for_session(&dry, None, Some(&es));
        let compact = plan_dry_compact_view(&dry, Some(&es));
        let commit_ref = es.mint_plan_commit_ref();
        let record = PlanCommitRecord {
            commit_ref: commit_ref.clone(),
            commit_id: compute_plan_commit_id_from_dry(&dry),
            domain_revision: es.domain_revision,
            artifact: bundle.artifact().clone(),
            program: trimmed.to_string(),
            dry_review: dry.review.clone(),
            verdict: compact.verdict,
            expires_at: Instant::now() + PLAN_COMMIT_TTL,
            dry_cache: PlanCommitDryCache::from_dry(&dry),
        };
        es.register_plan_commit(record);
        self.execute_session = Some(es);
        Ok(DryRunResult {
            plan_commit_ref: commit_ref.as_str().to_string(),
            summary,
            comp_json: serde_json::to_value(&bundle.artifact().comp)?,
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
            AGENT_SESSION_ID,
            &bundle,
            true,
            None,
            None,
            Some(dry),
        ))
        .await
        .map_err(|e| anyhow!("live execute failed: {e}"))?;

        let message = live
            .run_markdown
            .clone()
            .unwrap_or_else(|| format!("Live run completed for `{trimmed}`."));
        Ok(RunPlanResult {
            ok: true,
            message,
            rows_json: live_run_rows_json(&live),
            meta_json: live_run_meta_json(&live),
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
        let registry = InMemoryCgsRegistry::from_pairs(pairs);
        let config = ExecutionConfig::default();
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

    fn apply_exposure_wave(
        &mut self,
        cgs: Arc<CGS>,
        entry_id: &str,
        refs: &[&str],
        entities: &[String],
        use_intent: bool,
        intent_s: &str,
    ) -> Result<()> {
        if self.exposure.is_none() && self.catalogs.len() == 1 {
            if use_intent {
                let relation_keys = relation_endpoint_keys(entry_id, entities);
                let delta = derive_intent_exposure_surface_batch(
                    cgs.as_ref(),
                    entry_id,
                    intent_s,
                    &relation_keys,
                    entities,
                    None,
                    ExposureSurfaceOptions::default(),
                );
                self.exposure = Some(TeachingExposureSession::new_with_intent_delta(
                    cgs.as_ref(),
                    entry_id,
                    refs,
                    delta,
                ));
            } else {
                self.exposure = Some(TeachingExposureSession::new(cgs.as_ref(), entry_id, refs));
            }
        } else if self.exposure.is_some() {
            let layer_refs: Vec<&CGS> = self.catalogs.values().map(|a| a.as_ref()).collect();
            let exp = self.exposure.as_mut().expect("exposure");
            if use_intent {
                let relation_keys = exp.relation_endpoint_keys_for_wave(entry_id, entities);
                let delta = derive_intent_exposure_surface_batch(
                    cgs.as_ref(),
                    entry_id,
                    intent_s,
                    &relation_keys,
                    entities,
                    None,
                    ExposureSurfaceOptions::default(),
                );
                exp.expose_surface(&layer_refs, cgs, entry_id, refs, delta);
            } else {
                exp.expose_entities(&layer_refs, cgs, entry_id, refs);
            }
        } else if use_intent {
            let relation_keys = relation_endpoint_keys(entry_id, entities);
            let delta = derive_intent_exposure_surface_batch(
                cgs.as_ref(),
                entry_id,
                intent_s,
                &relation_keys,
                entities,
                None,
                ExposureSurfaceOptions::default(),
            );
            self.exposure = Some(TeachingExposureSession::new_with_intent_delta(
                cgs.as_ref(),
                entry_id,
                refs,
                delta,
            ));
        } else {
            self.exposure = Some(TeachingExposureSession::new(cgs.as_ref(), entry_id, refs));
        }
        Ok(())
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
        Ok(teaching_tsv_from_wrapped_prompt(
            &rendered,
            mode.markdown_fence_info_string(),
            TeachingFenceSlice::TableOnly,
        )
        .unwrap_or(rendered))
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
        Ok(ExecuteSession::new(
            AGENT_PROMPT_HASH.into(),
            AGENT_SESSION_ID.into(),
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
            None,
        ))
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
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec!["abc123".into()],
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
            node_results: Vec::new(),
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some("## done".into()),
            run_plasm_meta: Some(meta),
            return_steps: vec![step],
        }
    }

    #[test]
    fn load_expose_and_dry_run_execute_tiny() {
        let mut engine = AgentEngine::new();
        let info = engine
            .load_catalog(&execute_tiny_dir())
            .expect("load catalog");
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
            _base_url: &str,
            request: &plasm_compile::CompiledRequest,
            _auth: Option<plasm_runtime::auth::ResolvedAuth>,
        ) -> std::result::Result<
            (serde_json::Value, Option<String>),
            plasm_runtime::error::RuntimeError,
        > {
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
        let mut engine = AgentEngine::new();
        let info = engine
            .load_catalog(&execute_tiny_dir())
            .expect("load catalog");
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
    }
}
