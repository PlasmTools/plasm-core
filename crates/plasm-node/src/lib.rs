mod engine;
mod transport;
mod types;

pub use engine::{
    AgentEngine, CapabilityIntrospection, CatalogInfo, CatalogIntrospection, DiscoverResult,
    DryRunResult, EntityIntrospection, RunPlanResult, TeachingExposureResult,
};
pub use types::{JsTransportRequest, JsTransportResponse};

use engine::AgentEngine as InnerEngine;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use plasm_agent_core::http_execute::CapabilitySeed;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

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
    pub tsv: String,
    pub delta_refs: Vec<String>,
}

#[napi(object)]
pub struct JsDryRunResult {
    pub plan_commit_ref: String,
    pub summary: String,
    pub comp_json: String,
}

#[napi(object)]
pub struct JsDiscoverResult {
    pub markdown: String,
}

#[napi(object)]
pub struct JsRunPlanResult {
    pub ok: bool,
    pub message: String,
    #[napi(js_name = "rowsJson")]
    pub rows_json: Option<String>,
    #[napi(js_name = "metaJson")]
    pub meta_json: Option<String>,
}

fn map_err(err: anyhow::Error) -> Error {
    Error::from_reason(err.to_string())
}

#[napi]
pub struct PlasmEngine {
    inner: Arc<Mutex<InnerEngine>>,
}

impl Default for PlasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[napi]
impl PlasmEngine {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(InnerEngine::new())),
        }
    }

    #[napi]
    pub fn load_catalog(&self, catalog_dir: String) -> Result<JsCatalogInfo> {
        let mut engine = self.inner.blocking_lock();
        let info = engine
            .load_catalog(PathBuf::from(catalog_dir).as_path())
            .map_err(map_err)?;
        Ok(JsCatalogInfo {
            entry_id: info.entry_id,
            catalog_cgs_hash: info.catalog_cgs_hash,
        })
    }

    #[napi]
    pub fn expose_seeds(&self, intent: String, seeds: Vec<JsSeed>) -> Result<JsTeachingResult> {
        let mut engine = self.inner.blocking_lock();
        let capability_seeds: Vec<CapabilitySeed> = seeds
            .into_iter()
            .map(|s| CapabilitySeed {
                entry_id: s.api,
                entity: s.entity,
            })
            .collect();
        let result = engine
            .expose_seeds(intent, &capability_seeds)
            .map_err(map_err)?;
        Ok(JsTeachingResult {
            tsv: result.tsv,
            delta_refs: result.delta_refs,
        })
    }

    #[napi]
    pub fn introspect_catalog(&self, entry_id: String) -> Result<String> {
        let engine = self.inner.blocking_lock();
        let info = engine.introspect_catalog(&entry_id).map_err(map_err)?;
        serde_json::to_string(&info).map_err(|e| Error::from_reason(e.to_string()))
    }

    #[napi]
    pub fn dry_run(&self, program: String) -> Result<JsDryRunResult> {
        let mut engine = self.inner.blocking_lock();
        let result = engine.dry_run(&program).map_err(map_err)?;
        Ok(JsDryRunResult {
            plan_commit_ref: result.plan_commit_ref,
            summary: result.summary,
            comp_json: serde_json::to_string(&result.comp_json)
                .map_err(|e| Error::from_reason(e.to_string()))?,
        })
    }

    #[napi]
    pub fn discover(&self, intent: String) -> Result<JsDiscoverResult> {
        let engine = self.inner.blocking_lock();
        let result = engine.discover(&intent).map_err(map_err)?;
        Ok(JsDiscoverResult {
            markdown: result.markdown,
        })
    }

    #[napi]
    pub fn run_plan(&self, plan_commit_ref: String) -> Result<JsRunPlanResult> {
        let mut engine = self.inner.blocking_lock();
        let result = engine.run_plan(&plan_commit_ref).map_err(map_err)?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
        })
    }

    #[napi(
        ts_args_type = "planCommitRef: string, transport: (request: JsTransportRequest) => JsTransportResponse | Promise<JsTransportResponse>"
    )]
    pub async fn run_plan_live(
        &self,
        plan_commit_ref: String,
        transport: JsHostTransport,
    ) -> Result<JsRunPlanResult> {
        let callback_transport = {
            let engine = self.inner.lock().await;
            let entry_id = engine.primary_entry_id();
            JsCallbackHttpTransport::new(transport.0, entry_id)
        };
        let mut engine = self.inner.lock().await;
        let result = engine
            .run_plan_live(&plan_commit_ref, callback_transport)
            .await
            .map_err(map_err)?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
        })
    }
}
