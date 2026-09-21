#![recursion_limit = "512"]

mod engine;
mod tracing_setup;
mod transport;
mod types;

pub use engine::{
    AgentEngine, CapabilityIntrospection, CatalogInfo, CatalogIntrospection, DryRunResult,
    EntityIntrospection, RunPlanResult, TeachingExposureResult,
};
pub use types::{JsTransportRequest, JsTransportResponse};

use engine::AgentEngine as InnerEngine;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use plasm_agent_core::discovery_service::{DiscoveryService, RouteTurn};
use plasm_agent_core::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, PreparedCatalog,
};
use plasm_agent_core::http_execute::CapabilitySeed;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
    pub tsv: String,
    pub delta_refs: Vec<String>,
}

#[napi(object)]
pub struct JsDryRunResult {
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
}

fn map_err(err: anyhow::Error) -> Error {
    Error::from_reason(format!("{err:#}"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// In-process Plasm engine for NAPI.
///
/// Each engine has a FIFO asynchronous admission queue. A live HTTP callback may
/// await JavaScript while retaining its session engine; queued callers yield the
/// executor instead of blocking it. Independent logical sessions remain concurrent.
#[napi]
pub struct PlasmEngine {
    inner: Arc<AsyncMutex<InnerEngine>>,
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<InnerEngine>>>>,
    discovery_store: Arc<OnceCell<DiscoveryStore>>,
    activated: tokio::sync::RwLock<Option<(String, std::collections::BTreeSet<String>)>>,
}

impl Default for PlasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlasmEngine {
    fn session_engine(&self, id: &str) -> anyhow::Result<Arc<AsyncMutex<InnerEngine>>> {
        lock(&self.sessions).get(id).cloned().ok_or_else(|| {
            anyhow::anyhow!("unknown logical session `{id}`; open a session with plasm_context")
        })
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
    ) -> anyhow::Result<(
        String,
        DiscoveryAuthorization,
        Vec<plasm_core::prerequisites::CapabilityRef>,
    )> {
        if let Some(id) = logical_session_id {
            let shared = self.session_engine(id)?;
            let engine = shared.lock().await;
            let pin = engine.discovery_pin().ok_or_else(|| {
                anyhow::anyhow!("logical session has no discovery generation pin")
            })?;
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
                .ok_or_else(|| anyhow::anyhow!("no activated discovery generation"))?;
            Ok((
                generation,
                DiscoveryAuthorization::catalogs(allowed),
                Vec::new(),
            ))
        }
    }

    async fn refresh_discovery_session(&self, engine: &InnerEngine) -> anyhow::Result<()> {
        let pin = engine
            .discovery_pin()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("logical session has no discovery generation pin"))?;
        let store = self
            .discovery_store
            .get()
            .ok_or_else(|| anyhow::anyhow!("discovery store is unavailable"))?;
        store.refresh_session_pin(&pin).await
    }
}

#[napi]
impl PlasmEngine {
    #[napi(constructor)]
    pub fn new() -> Self {
        tracing_setup::init();
        Self {
            inner: Arc::new(AsyncMutex::new(InnerEngine::new())),
            sessions: Mutex::new(HashMap::new()),
            discovery_store: Arc::new(OnceCell::new()),
            activated: Default::default(),
        }
    }

    #[napi]
    pub async fn load_catalog(&self, catalog_dir: String) -> Result<JsCatalogInfo> {
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
        let (paths, allowed) = {
            let engine = self.inner.lock().await;
            (engine.packed_manifests(), engine.allowed_catalogs())
        };
        let bindings = serde_json::from_str(&bindings_json)
            .map_err(|e| Error::from_reason(format!("invalid deployment bindings: {e}")))?;
        let catalogs = paths
            .iter()
            .map(|p| PreparedCatalog::load(p))
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(map_err)?;
        let store = self
            .discovery_store
            .get_or_try_init(|| async {
                let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
                    .or_else(|_| std::env::var("DATABASE_URL"))
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "discovery requires PLASM_DISCOVERY_DATABASE_URL or DATABASE_URL"
                        )
                    })?;
                let store = DiscoveryStore::connect(&url).await?;
                store.migrate().await?;
                Ok::<_, anyhow::Error>(store)
            })
            .await
            .map_err(map_err)?;
        let generation = store
            .import(&deployment_id, catalogs, &bindings)
            .await
            .map_err(map_err)?;
        *self.activated.write().await = Some((generation.clone(), allowed));
        Ok(generation)
    }

    /// Typed intent provenance plus current affirmative effect slots; selection runs
    /// outside the execution mutex.
    #[napi]
    pub async fn route_intent(
        &self,
        intent_provenance_json: String,
        effect_slots: Vec<String>,
        logical_session_id: Option<String>,
    ) -> Result<String> {
        let provenance: plasm_agent_core::intent_provenance::IntentProvenance =
            serde_json::from_str(&intent_provenance_json)
                .map_err(|error| Error::from_reason(error.to_string()))?;
        let store = self.discovery_store.get().ok_or_else(|| {
            Error::from_reason(
                "activateDiscovery must validate a complete generation before routing",
            )
        })?;
        let (generation, allowed, exposed) = self
            .routing_inputs(logical_session_id.as_deref())
            .await
            .map_err(map_err)?;
        let service = DiscoveryService::from_env(store.clone()).map_err(map_err)?;
        let receipt = service
            .route_turn(RouteTurn {
                new_generation: &generation,
                intent_provenance: &provenance,
                effect_slots: &effect_slots,
                logical_session: logical_session_id.as_deref(),
                allowed: &allowed,
                exposed: &exposed,
                expires_at: std::time::SystemTime::now()
                    + std::time::Duration::from_secs(24 * 60 * 60),
            })
            .await
            .map_err(map_err)?;
        let teaching = {
            let (catalogs, compiled_catalogs, _) = store
                .load_generation(&receipt.retrieval.generation)
                .await
                .map_err(map_err)?;
            let pin = DiscoverySessionPin {
                authorization: receipt.authorization.clone(),
                generation: receipt.retrieval.generation.clone(),
                pin_id: receipt.pin_id.clone(),
            };
            let shared = {
                let mut sessions = lock(&self.sessions);
                sessions
                    .entry(receipt.pin_id.clone())
                    .or_insert_with(|| {
                        Arc::new(AsyncMutex::new(InnerEngine::from_pinned_generation(
                            catalogs,
                            compiled_catalogs,
                            pin.clone(),
                        )))
                    })
                    .clone()
            };
            let mut engine = shared.lock().await;
            if engine.discovery_pin() != Some(&pin) {
                return Err(Error::from_reason(
                    "routing receipt does not match the logical session pin",
                ));
            }
            receipt
                .closure
                .as_ref()
                .map(|closure| {
                    engine
                        .expose_routing(&receipt.intent, closure)
                        .map_err(map_err)
                })
                .transpose()?
        };
        serde_json::to_string(&serde_json::json!({"routing":receipt,"teaching":teaching}))
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn expose_seeds(
        &self,
        intent: String,
        seeds: Vec<JsSeed>,
    ) -> Result<JsTeachingResult> {
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
            .map_err(map_err)?;
        Ok(JsTeachingResult {
            tsv: result.tsv,
            delta_refs: result.delta_refs,
        })
    }

    #[napi]
    pub async fn introspect_catalog(&self, entry_id: String) -> Result<String> {
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
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let result = engine.dry_run(&program).map_err(map_err)?;
        Ok(JsDryRunResult {
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
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let result = engine.run_plan(&plan_commit_ref).map_err(map_err)?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
            artifacts_json: result.artifacts_json,
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
        let mut engine = self.execution_engine(logical_session_id.as_deref()).await?;
        let callback = JsCallbackHttpTransport::new(transport.0, engine.primary_entry_id());
        let result = engine
            .run_plan_live(&plan_commit_ref, callback)
            .await
            .map_err(map_err)?;
        Ok(JsRunPlanResult {
            ok: result.ok,
            message: result.message,
            rows_json: result.rows_json,
            meta_json: result.meta_json,
            artifacts_json: result.artifacts_json,
        })
    }
}

#[cfg(test)]
mod mutex_law_tests {
    use super::*;

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
            lock(&engine.sessions).insert(id.into(), Arc::new(AsyncMutex::new(InnerEngine::new())));
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
