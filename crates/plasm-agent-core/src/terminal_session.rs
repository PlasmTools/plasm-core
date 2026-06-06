//! Client-owned symbol sessions: catalog fetch, local `TeachingExposureSession`, teaching TSV rendering.

use anyhow::{anyhow, Context as _, Result};
use indexmap::IndexMap;
use plasm_core::prompt_render::teaching_tsv_table_from_wrapped_prompt;
use plasm_core::CgsContext;
use plasm_core::{
    discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions},
    relation_endpoint_keys, TeachingExposureSession, ExposureEntityKey, PromptPipelineConfig,
    SymbolMapCrossRequestCache, CGS,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::http_execute::{build_capability_exposure_plan, CapabilitySeed};
use crate::plasm_dag::compile_plasm_expression_to_plan;
use crate::plasm_plan_run::{expand_program_surface_for_session_lower, parse_plasm_surface_line};
use crate::terminal_state::{
    append_teaching_tsv_wave, catalog_cache_path, teaching_tsv_path, write_session_meta, CatalogPin,
    ExecutionBinding, SessionMeta,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolStateFile {
    pub version: u32,
    pub client_session_id: String,
    pub intent: String,
    pub capabilities: Vec<(String, String)>,
    pub catalogs: Vec<CatalogPin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionBinding>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntryWithCgs {
    #[allow(dead_code)]
    entry_id: String,
    #[serde(default)]
    catalog_cgs_hash: Option<String>,
    cgs: Option<CGS>,
}

/// In-memory client symbol authority.
pub struct ClientSymbolSession {
    pub client_session_id: String,
    pub intent: String,
    pub capabilities: Vec<(String, String)>,
    pub catalogs: IndexMap<String, Arc<CGS>>,
    pub catalog_digests: IndexMap<String, String>,
    exposure: Option<TeachingExposureSession>,
    pub execution: Option<ExecutionBinding>,
    pipeline: PromptPipelineConfig,
    sym_cross: SymbolMapCrossRequestCache,
}

impl ClientSymbolSession {
    pub fn new(client_session_id: String, intent: String) -> Self {
        Self {
            client_session_id,
            intent,
            capabilities: Vec::new(),
            catalogs: IndexMap::new(),
            catalog_digests: IndexMap::new(),
            exposure: None,
            execution: None,
            pipeline: PromptPipelineConfig::default(),
            sym_cross: SymbolMapCrossRequestCache::from_env(),
        }
    }

    pub fn has_capability(&self, api: &str, entity: &str) -> bool {
        self.capabilities
            .iter()
            .any(|(a, e)| a == api && e == entity)
    }

    pub fn catalog_pins(&self) -> Vec<CatalogPin> {
        self.catalog_digests
            .iter()
            .map(|(api, digest)| CatalogPin {
                api: api.clone(),
                digest: digest.clone(),
            })
            .collect()
    }

    pub fn to_symbol_state_file(&self) -> SymbolStateFile {
        SymbolStateFile {
            version: 1,
            client_session_id: self.client_session_id.clone(),
            intent: self.intent.clone(),
            capabilities: self.capabilities.clone(),
            catalogs: self.catalog_pins(),
            execution: self.execution.clone(),
        }
    }

    pub fn persist(&self, server: &str) -> Result<()> {
        let path = crate::terminal_state::symbol_state_path(server, &self.client_session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&self.to_symbol_state_file())?,
        )?;
        let meta = SessionMeta {
            client_session_id: self.client_session_id.clone(),
            intent: self.intent.clone(),
            capabilities: self.capabilities.clone(),
            catalogs: self.catalog_pins(),
            execution: self.execution.clone(),
        };
        write_session_meta(server, &meta)?;
        Ok(())
    }

    pub fn write_catalog_cache(&self, server: &str, api: &str, cgs: &CGS) -> Result<()> {
        let path = catalog_cache_path(server, &self.client_session_id, api);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec(cgs)?)?;
        Ok(())
    }

    pub fn load_catalog_cache(server: &str, client_session_id: &str, api: &str) -> Result<CGS> {
        let path = catalog_cache_path(server, client_session_id, api);
        let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).map_err(|e| anyhow!("catalog cache parse: {e}"))
    }

    /// Fetch CGS from `GET /v1/registry/{entry_id}?include_cgs=true`, cache locally, pin digest.
    pub async fn ensure_catalog(
        &mut self,
        client: &Client,
        server: &str,
        profile: &crate::terminal::TerminalProfileRef<'_>,
        entry_id: &str,
    ) -> Result<()> {
        if self.catalogs.contains_key(entry_id) {
            return Ok(());
        }
        let url = format!(
            "{}/v1/registry/{}?include_cgs=true",
            server.trim_end_matches('/'),
            entry_id
        );
        let mut headers = HeaderMap::new();
        profile.apply_auth_headers(&mut headers)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let res = client.get(&url).headers(headers).send().await?;
        let st = res.status();
        let body = res.bytes().await?.to_vec();
        if !st.is_success() {
            return Err(anyhow!(
                "catalog export for `{entry_id}`: HTTP {st}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        let entry: RegistryEntryWithCgs =
            serde_json::from_slice(&body).map_err(|e| anyhow!("catalog export JSON: {e}"))?;
        let cgs = entry
            .cgs
            .ok_or_else(|| anyhow!("catalog export for `{entry_id}`: missing cgs field"))?;
        let digest = entry
            .catalog_cgs_hash
            .unwrap_or_else(|| cgs.catalog_cgs_hash_hex());
        if let Some(existing) = self.catalog_digests.get(entry_id) {
            if existing != &digest {
                return Err(anyhow!(
                    "catalog digest changed for `{entry_id}` — run `plasm context --new` to start a fresh symbol space"
                ));
            }
        }
        self.write_catalog_cache(server, entry_id, &cgs)?;
        self.catalog_digests.insert(entry_id.to_string(), digest);
        self.catalogs.insert(entry_id.to_string(), Arc::new(cgs));
        Ok(())
    }

    #[allow(dead_code)]
    fn cgs_layers(&self) -> Vec<&CGS> {
        self.catalogs.values().map(|c| c.as_ref()).collect()
    }

    fn by_entry_cgs(&self) -> IndexMap<String, &CGS> {
        self.catalogs
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref()))
            .collect()
    }

    /// Expose new seeds; returns TSV fragment rows appended (empty if noop).
    pub fn expose_seeds(&mut self, seeds: &[CapabilitySeed]) -> Result<String> {
        let mut newly_added: Vec<CapabilitySeed> = Vec::new();
        for s in seeds {
            if !self.has_capability(&s.entry_id, &s.entity) {
                newly_added.push(s.clone());
                self.capabilities
                    .push((s.entry_id.clone(), s.entity.clone()));
            }
        }
        if newly_added.is_empty() {
            return Ok(String::new());
        }
        // Server execute binding was opened for a narrower capability set; rebuild before next run.
        self.execution = None;

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
        let intent_s = self.intent.trim();
        let use_intent = !intent_s.is_empty();

        let mut process_order: Vec<String> = grouped.keys().cloned().collect();
        process_order.sort();

        for entry_id in &process_order {
            let Some(entities) = grouped.get(entry_id) else {
                continue;
            };
            let cgs = self
                .catalogs
                .get(entry_id)
                .ok_or_else(|| anyhow!("missing catalog `{entry_id}` in client session"))?;
            let refs: Vec<&str> = entities.iter().map(|s| s.as_str()).collect();
            let n0 = self
                .exposure
                .as_ref()
                .map(|e| e.entities.len())
                .unwrap_or(0);

            if self.exposure.is_none() && self.catalogs.len() == 1 {
                if use_intent {
                    let relation_keys = relation_endpoint_keys(entry_id.as_str(), entities);
                    let delta = derive_intent_exposure_surface_batch(
                        cgs.as_ref(),
                        entry_id.as_str(),
                        intent_s,
                        &relation_keys,
                        entities,
                        None,
                        ExposureSurfaceOptions::default(),
                    );
                    self.exposure = Some(TeachingExposureSession::new_with_intent_delta(
                        cgs.as_ref(),
                        entry_id.as_str(),
                        &refs,
                        delta,
                    ));
                } else {
                    self.exposure = Some(TeachingExposureSession::new(
                        cgs.as_ref(),
                        entry_id.as_str(),
                        &refs,
                    ));
                }
            } else if self.exposure.is_some() {
                let layer_refs: Vec<&CGS> = self.catalogs.values().map(|a| a.as_ref()).collect();
                let exp = self.exposure.as_mut().expect("exposure");
                if use_intent {
                    let relation_keys =
                        exp.relation_endpoint_keys_for_wave(entry_id.as_str(), entities);
                    let delta = derive_intent_exposure_surface_batch(
                        cgs.as_ref(),
                        entry_id.as_str(),
                        intent_s,
                        &relation_keys,
                        entities,
                        None,
                        ExposureSurfaceOptions::default(),
                    );
                    exp.expose_surface(&layer_refs, cgs.clone(), entry_id.as_str(), &refs, delta);
                } else {
                    exp.expose_entities(&layer_refs, cgs.clone(), entry_id.as_str(), &refs);
                }
            } else if use_intent {
                let relation_keys = relation_endpoint_keys(entry_id.as_str(), entities);
                let delta = derive_intent_exposure_surface_batch(
                    cgs.as_ref(),
                    entry_id.as_str(),
                    intent_s,
                    &relation_keys,
                    entities,
                    None,
                    ExposureSurfaceOptions::default(),
                );
                self.exposure = Some(TeachingExposureSession::new_with_intent_delta(
                    cgs.as_ref(),
                    entry_id.as_str(),
                    &refs,
                    delta,
                ));
            } else {
                self.exposure = Some(TeachingExposureSession::new(
                    cgs.as_ref(),
                    entry_id.as_str(),
                    &refs,
                ));
            }

            let exp = self
                .exposure
                .as_ref()
                .ok_or_else(|| anyhow!("exposure missing after expose"))?;
            let added_qualified = exp.qualified_entities_since(n0);
            all_new_qualified.extend(added_qualified);
        }

        if all_new_qualified.is_empty() {
            return Ok(String::new());
        }

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
            self.pipeline
                .render_teaching_exposure_delta(cgs, exp, &added_refs, Some(&self.sym_cross))
        } else {
            self.pipeline.render_teaching_exposure_delta_federated(
                &by_entry,
                exp,
                &all_new_qualified,
                Some(&self.sym_cross),
            )
        };

        let mode = self.pipeline.render_mode;
        let tsv =
            teaching_tsv_table_from_wrapped_prompt(&rendered, mode.markdown_fence_info_string())
                .unwrap_or(rendered);
        Ok(tsv)
    }

    /// Rebuild exposure from persisted capabilities (after loading catalog caches).
    pub fn rebuild_exposure_from_capabilities(&mut self) -> Result<()> {
        let seeds: Vec<CapabilitySeed> = self
            .capabilities
            .iter()
            .map(|(api, entity)| CapabilitySeed {
                entry_id: api.clone(),
                entity: entity.clone(),
            })
            .collect();
        self.exposure = None;
        self.capabilities.clear();
        if seeds.is_empty() {
            return Ok(());
        }
        let _ = self.expose_seeds(&seeds)?;
        Ok(())
    }

    pub fn load_from_disk(server: &str, client_session_id: &str) -> Result<Self> {
        let path = crate::terminal_state::symbol_state_path(server, client_session_id);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file: SymbolStateFile =
            serde_json::from_str(&raw).map_err(|e| anyhow!("symbols.json: {e}"))?;
        let mut sess = ClientSymbolSession::new(file.client_session_id, file.intent);
        sess.capabilities = file.capabilities;
        sess.execution = file.execution;
        for pin in &file.catalogs {
            let cgs = Self::load_catalog_cache(server, &sess.client_session_id, &pin.api)?;
            if cgs.catalog_cgs_hash_hex() != pin.digest {
                return Err(anyhow!(
                    "catalog digest mismatch for `{}` — run `plasm context --new`",
                    pin.api
                ));
            }
            sess.catalog_digests
                .insert(pin.api.clone(), pin.digest.clone());
            sess.catalogs.insert(pin.api.clone(), Arc::new(cgs));
        }
        sess.rebuild_exposure_from_capabilities()?;
        Ok(sess)
    }

    pub fn append_rendered_tsv(
        &self,
        server: &str,
        tsv_fragment: &str,
    ) -> Result<(PathBuf, usize)> {
        let path = teaching_tsv_path(server, &self.client_session_id);
        let first = !path.exists() || path.metadata().map(|m| m.len()).unwrap_or(0) == 0;
        let rows = append_teaching_tsv_wave(&path, tsv_fragment, first)?;
        Ok((path, rows))
    }

    /// Build a minimal [`ExecuteSession`] for client-side parse/expand (symbol authority stays local).
    pub fn build_execute_session_for_parse(&self) -> Result<ExecuteSession> {
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
        let exposure_plan = build_capability_exposure_plan(&seeds)
            .ok_or_else(|| anyhow!("no catalogs in client session"))?;
        let primary_api = exposure_plan.primary_entry_id;
        let cgs = self
            .catalogs
            .get(&primary_api)
            .ok_or_else(|| anyhow!("missing primary cgs"))?
            .clone();
        let exposure = self.exposure.clone().ok_or_else(|| {
            anyhow!("client session has no symbol exposure — run `plasm context` first")
        })?;
        let entities = exposure.entities.clone();
        let catalog_cgs_hash = cgs.catalog_cgs_hash_hex();
        Ok(ExecuteSession::new(
            String::new(),
            String::new(),
            cgs,
            contexts_by_entry,
            primary_api,
            String::new(),
            String::new(),
            None,
            entities,
            Some(exposure),
            None,
            None,
            catalog_cgs_hash,
            Some(self.intent.clone()),
            None,
        ))
    }

    /// Lower surface or program text to serialized plan JSON using local symbol authority.
    pub fn compile_program_to_plan(&self, source: &str) -> Result<serde_json::Value> {
        let es = self.build_execute_session_for_parse()?;
        let pipeline = &self.pipeline;
        let cross = Some(&self.sym_cross);
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("program is empty"));
        }
        compile_plasm_expression_to_plan(pipeline, cross, &es, "plasm_cli", trimmed)
            .map_err(|e| anyhow!("{e}"))
    }

    #[allow(dead_code)]
    pub fn expand_program_line(&self, line: &str) -> Result<String> {
        let session = self.build_execute_session_for_parse()?;
        Ok(expand_program_surface_for_session_lower(
            &session,
            &self.pipeline,
            line.trim(),
        ))
    }

    #[allow(dead_code)]
    pub fn parse_line_for_display(&self, line: &str) -> Result<String> {
        let session = self.build_execute_session_for_parse()?;
        let parsed =
            parse_plasm_surface_line(&session, Some(&self.sym_cross), &self.pipeline, line.trim())?;
        if session.contexts_by_entry.len() <= 1 {
            Ok(crate::expr_display::expr_display_resolved(
                &parsed.expr,
                session.cgs.as_ref(),
            ))
        } else {
            let fed = session
                .federation_dispatch()
                .ok_or_else(|| anyhow!("federation dispatch missing"))?;
            Ok(crate::expr_display::expr_display_resolved_federated(
                &parsed.expr,
                fed.as_ref(),
                session.cgs.as_ref(),
            ))
        }
    }
}

use std::path::PathBuf;

// Re-export profile type for ensure_catalog signature
impl ClientSymbolSession {
    #[allow(dead_code)]
    pub fn exposed_apis(&self) -> HashSet<&str> {
        self.capabilities.iter().map(|(a, _)| a.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;
    use std::path::Path;

    #[test]
    fn compile_profile_query_locally() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let digest = cgs.catalog_cgs_hash_hex();
        let mut sess = ClientSymbolSession::new("cs_test".into(), "profile query".into());
        sess.catalogs.insert("overshow".into(), cgs);
        sess.catalog_digests.insert("overshow".into(), digest);
        sess.capabilities
            .push(("overshow".into(), "Profile".into()));
        sess.rebuild_exposure_from_capabilities().expect("expose");
        let plan = sess
            .compile_program_to_plan("Profile{}")
            .expect("compile Profile{}");
        assert!(plan.get("nodes").and_then(|n| n.as_array()).is_some());
    }
}
