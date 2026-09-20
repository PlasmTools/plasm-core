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
use tokio::sync::OnceCell;

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

fn engine_busy() -> Error {
    Error::from_reason("engine busy in live run")
}

/// Occupancy of the default engine or one logical session.
/// `Live` is fail-closed: no blank `InnerEngine::new()` stand-in.
enum EngineSlot {
    Ready(InnerEngine),
    Live,
}

impl EngineSlot {
    fn ready_mut(&mut self) -> Result<&mut InnerEngine> {
        match self {
            EngineSlot::Ready(engine) => Ok(engine),
            EngineSlot::Live => Err(engine_busy()),
        }
    }

    fn ready_ref(&self) -> Result<&InnerEngine> {
        match self {
            EngineSlot::Ready(engine) => Ok(engine),
            EngineSlot::Live => Err(engine_busy()),
        }
    }

    fn take_ready(&mut self) -> Result<InnerEngine> {
        match std::mem::replace(self, EngineSlot::Live) {
            EngineSlot::Ready(engine) => Ok(engine),
            EngineSlot::Live => {
                *self = EngineSlot::Live;
                Err(engine_busy())
            }
        }
    }
}

/// Restores a detached engine on Drop (panic, Promise cancel, or Result).
struct LiveEngineGuard {
    inner: Option<Arc<Mutex<EngineSlot>>>,
    session: Option<(Arc<Mutex<HashMap<String, EngineSlot>>>, String)>,
    engine: Option<InnerEngine>,
}

impl LiveEngineGuard {
    fn session(
        map: Arc<Mutex<HashMap<String, EngineSlot>>>,
        id: String,
        engine: InnerEngine,
    ) -> Self {
        Self {
            inner: None,
            session: Some((map, id)),
            engine: Some(engine),
        }
    }

    fn default_inner(inner: Arc<Mutex<EngineSlot>>, engine: InnerEngine) -> Self {
        Self {
            inner: Some(inner),
            session: None,
            engine: Some(engine),
        }
    }

    fn engine_mut(&mut self) -> &mut InnerEngine {
        self.engine.as_mut().expect("live engine present")
    }
}

impl Drop for LiveEngineGuard {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            if let Some((map, id)) = self.session.take() {
                lock(&map).insert(id, EngineSlot::Ready(engine));
            } else if let Some(inner) = self.inner.take() {
                *lock(&inner) = EngineSlot::Ready(engine);
            }
        }
    }
}

/// In-process Plasm engine for NAPI.
///
/// **Mutex law (full cutover):** `inner` / `sessions` are `std::sync::Mutex`
/// held only for short critical sections. Never hold them across JS host-transport
/// awaits. Occupancy is `Ready | Live` — concurrent live callers fail closed.
#[napi]
pub struct PlasmEngine {
    inner: Arc<Mutex<EngineSlot>>,
    sessions: Arc<Mutex<HashMap<String, EngineSlot>>>,
    discovery_store: Arc<OnceCell<DiscoveryStore>>,
    activated: tokio::sync::RwLock<Option<(String, std::collections::BTreeSet<String>)>>,
}

impl Default for PlasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlasmEngine {
    async fn routing_inputs(
        &self,
        logical_session_id: Option<&str>,
    ) -> anyhow::Result<(
        String,
        DiscoveryAuthorization,
        Vec<plasm_core::prerequisites::CapabilityRef>,
    )> {
        if let Some(id) = logical_session_id {
            let sessions = lock(&self.sessions);
            let engine = sessions
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("unknown logical session"))?
                .ready_ref()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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

    async fn refresh_discovery_session(&self, id: &str) -> anyhow::Result<()> {
        let pin = lock(&self.sessions)
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown logical session"))?
            .ready_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?
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
            inner: Arc::new(Mutex::new(EngineSlot::Ready(InnerEngine::new()))),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            discovery_store: Arc::new(OnceCell::new()),
            activated: Default::default(),
        }
    }

    #[napi]
    pub async fn load_catalog(&self, catalog_dir: String) -> Result<JsCatalogInfo> {
        let mut slot = lock(&self.inner);
        let info = slot
            .ready_mut()?
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
            let slot = lock(&self.inner);
            let engine = slot.ready_ref()?;
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
            let mut sessions = lock(&self.sessions);
            let pin = DiscoverySessionPin {
                authorization: receipt.authorization.clone(),
                generation: receipt.retrieval.generation.clone(),
                pin_id: receipt.pin_id.clone(),
            };
            if let std::collections::hash_map::Entry::Vacant(vacant) =
                sessions.entry(receipt.pin_id.clone())
            {
                vacant.insert(EngineSlot::Ready(InnerEngine::from_pinned_generation(
                    catalogs,
                    compiled_catalogs,
                    pin.clone(),
                )));
            }
            let engine = sessions
                .get_mut(&receipt.pin_id)
                .ok_or_else(|| Error::from_reason("unknown logical session"))?
                .ready_mut()?;
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
        let mut slot = lock(&self.inner);
        let engine = slot.ready_mut()?;
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
        let slot = lock(&self.inner);
        let info = slot
            .ready_ref()?
            .introspect_catalog(&entry_id)
            .map_err(map_err)?;
        serde_json::to_string(&info).map_err(|e| Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn dry_run(
        &self,
        program: String,
        logical_session_id: Option<String>,
    ) -> Result<JsDryRunResult> {
        let result = if let Some(id) = logical_session_id {
            self.refresh_discovery_session(&id).await.map_err(map_err)?;
            lock(&self.sessions)
                .get_mut(&id)
                .ok_or_else(|| Error::from_reason("unknown logical session"))?
                .ready_mut()?
                .dry_run(&program)
                .map_err(map_err)?
        } else {
            lock(&self.inner)
                .ready_mut()?
                .dry_run(&program)
                .map_err(map_err)?
        };
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
        let result = if let Some(id) = logical_session_id {
            self.refresh_discovery_session(&id).await.map_err(map_err)?;
            lock(&self.sessions)
                .get_mut(&id)
                .ok_or_else(|| Error::from_reason("unknown logical session"))?
                .ready_mut()?
                .run_plan(&plan_commit_ref)
                .map_err(map_err)?
        } else {
            lock(&self.inner)
                .ready_mut()?
                .run_plan(&plan_commit_ref)
                .map_err(map_err)?
        };
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
        // Detach under a short std mutex, then await JS transport. Drop restores
        // Ready even if the NAPI Promise is cancelled.
        let result = if let Some(id) = logical_session_id {
            eprintln!("plasm-node: run_plan_live session={id}");
            self.refresh_discovery_session(&id).await.map_err(map_err)?;
            let engine = {
                let mut sessions = lock(&self.sessions);
                sessions
                    .get_mut(&id)
                    .ok_or_else(|| Error::from_reason("unknown logical session"))?
                    .take_ready()?
            };
            eprintln!("plasm-node: detached session engine");
            let mut guard = LiveEngineGuard::session(Arc::clone(&self.sessions), id, engine);
            let callback =
                JsCallbackHttpTransport::new(transport.0, guard.engine_mut().primary_entry_id());
            let run = guard
                .engine_mut()
                .run_plan_live(&plan_commit_ref, callback)
                .await
                .map_err(map_err)?;
            eprintln!("plasm-node: session live returned ok={}", run.ok);
            run
        } else {
            let engine = lock(&self.inner).take_ready()?;
            let mut guard = LiveEngineGuard::default_inner(Arc::clone(&self.inner), engine);
            let callback =
                JsCallbackHttpTransport::new(transport.0, guard.engine_mut().primary_entry_id());
            guard
                .engine_mut()
                .run_plan_live(&plan_commit_ref, callback)
                .await
                .map_err(map_err)?
        };
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
            EngineSlot::Ready(InnerEngine::from_pinned_generation(
                Default::default(),
                Default::default(),
                pin,
            )),
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
                .refresh_discovery_session("logical-session")
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

    #[test]
    fn live_guard_restores_ready_on_drop() {
        let map: Arc<Mutex<HashMap<String, EngineSlot>>> = Arc::new(Mutex::new(HashMap::new()));
        let pin = DiscoverySessionPin {
            generation: "g".into(),
            authorization: DiscoveryAuthorization {
                catalogs: ["c".into()].into(),
                capabilities: Default::default(),
            },
            pin_id: "s".into(),
        };
        let engine =
            InnerEngine::from_pinned_generation(Default::default(), Default::default(), pin);
        lock(&map).insert("s".into(), EngineSlot::Live);
        {
            let _guard = LiveEngineGuard::session(Arc::clone(&map), "s".into(), engine);
        }
        assert!(
            matches!(lock(&map).get("s"), Some(EngineSlot::Ready(_))),
            "Drop must restore Ready, not leave Live"
        );
    }

    #[test]
    fn run_plan_live_detaches_via_occupancy_and_drop_guard() {
        let src = include_str!("lib.rs");
        let live = src
            .split("pub async fn run_plan_live")
            .nth(1)
            .expect("run_plan_live");
        let live = live
            .split("Ok(JsRunPlanResult")
            .next()
            .expect("live result");
        assert!(
            live.contains("take_ready") && live.contains("LiveEngineGuard"),
            "run_plan_live must detach Ready→Live and restore via Drop"
        );
        assert!(
            !live.contains("InnerEngine::new()"),
            "run_plan_live must not park a blank engine while live"
        );
    }
}
