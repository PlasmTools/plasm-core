//! Shared host state for HTTP (`/v1/*`, `/execute`) and MCP (Streamable HTTP): registry, engine,
//! session store. Execute graph state lives on each [`crate::execute_session::ExecuteSession`]. CGS for each request comes from the registry / session, not
//! from a separate “default schema” field on this struct.
//!
//! The surface is split for OSS vs hosted SaaS: [`PlasmOssHostState`] is the data-plane / executor
//! (discovery, execute, traces, optional incoming-auth for execute identity). [`PlasmSaaSHostExtension`]
//! holds MCP policy sqlx + transport API keys and (when hosted) auth-framework + tenant binding.
//! OSS `plasm-mcp` may populate **only** the MCP repository + API key registry. Outbound OAuth KV/catalog live on
//! [`PlasmOssHostState`] so OSS HTTP can expose the same routes without pulling in Phoenix.

use crate::catalog_runtime::CatalogRuntime;
use crate::discovery_embedding_repository::DiscoveryEmbeddingRepository;
use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use crate::execute_session::{ExecuteSession, ExecuteSessionStore, SessionReuseKey};
use crate::incoming_auth::IncomingAuthVerifier;
use crate::incoming_auth_device::IncomingAuthDeviceStore;
use crate::local_trace_archive::LocalTraceArchive;
use crate::mcp_config_repository::McpConfigRepository;
use crate::mcp_transport_auth::McpTransportAuth;
use crate::mcp_transport_store::{
    ExecuteSessionRegistry, LogicalExecuteBindingRegistry, LogicalSymbolLedgerRegistry,
};
use crate::oauth_link_catalog::OauthLinkCatalog;
use crate::operation_persist::OperationPersistScheduler;
use crate::operation_progress::OperationProgressHub;
use crate::run_artifacts::RunArtifactStore;
use crate::session_coordination::SessionCoordination;
use crate::session_graph_persistence::SessionGraphPersistence;
use crate::session_identity::LogicalSessionRegistry;
use crate::tenant_binding::TenantBindingStore;
use crate::trace_hub::{TraceHub, TraceHubConfig};
use crate::trace_sink_emit::TraceIngestClient;
use auth_framework::storage::AuthStorage;
use auth_framework::AuthFramework;
use dashmap::DashMap;
use plasm_discovery::embedding_store::CatalogEmbeddingStore;
use plasm_discovery::CatalogIndexCache;
use plasm_runtime::{EnvSecretProvider, ExecutionEngine, ExecutionMode, SecretProvider};
use std::ops::Deref;
use std::sync::Arc;
use uuid::Uuid;

pub use crate::catalog_runtime::CatalogBootstrap;

/// Open-source / data-plane state: engine, registry-backed catalog, execute sessions, traces.
///
/// MCP policy rows and API key material are **not** fields here; when enabled they attach via
/// [`super::PlasmHostState::saas`] (`mcp_config_repository`, `mcp_transport_auth`).
#[derive(Clone)]
pub struct PlasmOssHostState {
    pub engine: Arc<ExecutionEngine>,
    pub mode: ExecutionMode,
    /// Swappable catalog snapshot, bootstrap mode, and reload generation — see [`CatalogRuntime`](crate::catalog_runtime::CatalogRuntime).
    pub catalog: CatalogRuntime,
    pub sessions: Arc<ExecuteSessionStore>,
    /// Logical session minting for MCP `plasm_context` (Redis-backed when transport store is wired).
    pub logical_sessions: Arc<LogicalSessionRegistry>,
    /// Latest execute binding per logical session: `logical_session_id` → `(prompt_hash, execute_session_id)`.
    /// Used for MCP `resources/read` on `plasm://session/{uuid}/r/{n}` without relying on transport state.
    pub logical_execute_bindings: LogicalExecuteBindingRegistry,
    /// Append-only `e#`/`m#`/`p#`/`r#` ledger per logical session (survives transport row churn).
    pub logical_symbol_ledgers: LogicalSymbolLedgerRegistry,
    /// Durable execute-session descriptors for cross-pod rehydration (`PLASM_MCP_TRANSPORT_REDIS_URL`).
    pub execute_session_registry: ExecuteSessionRegistry,
    /// Stored execute run snapshots (`GET .../artifacts/:run_id`, MCP `resources/read`). See [`crate::run_artifacts`].
    pub run_artifacts: Arc<RunArtifactStore>,
    /// Optional object-store-backed delta/snapshot persistence for session graph state.
    pub session_graph_persistence: Option<Arc<SessionGraphPersistence>>,
    /// When set, HTTP routes run [`crate::incoming_auth::incoming_auth_http_middleware`].
    pub incoming_auth: Option<Arc<IncomingAuthVerifier>>,
    /// CLI device-login marker ([`crate::incoming_auth_device`] sessions live in auth-framework KV).
    pub incoming_auth_device: Arc<IncomingAuthDeviceStore>,
    /// MCP transport session traces (demo/debug; in-memory).
    pub trace_hub: Arc<TraceHub>,
    /// Queued MCP `notifications/plasm/op` payloads (drained by MCP session reporter).
    pub op_progress_hub: Arc<OperationProgressHub>,
    /// Debounced cross-pod async operation descriptor persistence.
    pub operation_persist: Arc<OperationPersistScheduler>,
    /// Effective [`TraceHubConfig`] after startup (matches [`TraceHub::bounds`] on the hub).
    pub trace_hub_config: TraceHubConfig,
    /// Best-effort POST of audit batches to the trace sink (`PLASM_TRACE_SINK_URL` when using [`EnvTraceIngestClient`]).
    pub trace_ingest: Arc<dyn TraceIngestClient>,
    /// Optional local on-disk history (`PLASM_TRACE_ARCHIVE_DIR`) for OSS self-host durable reads.
    pub local_trace_archive: Option<Arc<LocalTraceArchive>>,
    /// Trace sink read API base URL (defaults to `PLASM_TRACE_SINK_URL` when unset). Highest precedence for durable list/detail.
    pub trace_sink_read_base_url: Option<String>,
    /// Reused HTTP client for trace sink read proxy (`GET /v1/traces*`).
    pub trace_sink_http: reqwest::Client,
    /// Auth-framework KV store for outbound OAuth pending sessions and `hosted_kv` secrets (optional on OSS).
    pub auth_storage: Option<Arc<dyn AuthStorage>>,
    /// OAuth2 catalog for outbound account linking (`/internal/oauth-link/...`, `/oauth/link/callback`).
    pub oauth_link_catalog: Option<Arc<OauthLinkCatalog>>,
    /// Hosted KV + catalog outbound resolver for `hosted_kv` in CGS.
    pub outbound_secret_provider: Option<Arc<dyn SecretProvider>>,
    /// Optional Postgres-backed typed-discovery embeddings (CGS `catalog_cgs_hash` rows).
    pub discovery_embedding: Option<Arc<DiscoveryEmbeddingRepository>>,
    /// Memoized [`CatalogIndex`](plasm_discovery::index::CatalogIndex) per `(entry_id, catalog_cgs_hash)`.
    pub discovery_index_cache: Arc<CatalogIndexCache>,
    /// Shared ONNX embedder for typed discovery + background reconcile (`local-embeddings` only).
    #[cfg(feature = "local-embeddings")]
    pub discovery_embedder: Arc<plasm_discovery::BlockingEmbedder>,
    /// Tenant workflow manifests for MCP Apps (`GET /v1/workflows/:id/view-model`).
    pub workflows: Arc<crate::workflow_registry::WorkflowRegistry>,
    /// Shared Redis backend for MCP transport + execute session externalization (when configured).
    pub redis_backend: Option<Arc<crate::mcp_transport_store::RedisBackend>>,
    /// Large-stack worker pool for live `run_plasm_comp` (injected at bootstrap; shared via `Arc`).
    pub live_plan_pool: Arc<crate::live_plan_run_worker::LivePlanRunPool>,
    /// CEP-13: single-flight logical open + serialized teaching exposure commits per execute row.
    pub session_coordination: Arc<SessionCoordination>,
    /// HTTP `User-Agent` per MCP transport session id (`mcp-session-id` header).
    pub mcp_http_user_agents: Arc<DashMap<String, String>>,
}

/// Hosted / control-plane state: same process as [`PlasmOssHostState`], but injected after OSS bootstrap.
#[derive(Clone)]
pub struct PlasmSaaSHostExtension {
    /// Initialized in HTTP/MCP mode when the hosted bundle is enabled.
    pub auth_framework: Option<Arc<tokio::sync::Mutex<AuthFramework>>>,
    /// Tenant MCP configuration (sqlx Postgres). When `None`, MCP bind/policy is disabled.
    pub mcp_config_repository: Option<Arc<McpConfigRepository>>,
    /// Streamable HTTP MCP: API key verification (backed by [`AuthStorage`]).
    pub mcp_transport_auth: Option<Arc<dyn McpTransportAuth>>,
    /// Incoming-auth subject → tenant + workspace/project slugs (Postgres).
    pub tenant_binding: Option<Arc<TenantBindingStore>>,
}

/// Full in-process state for the `plasm-mcp` **hosted** build: data plane plus optional control-plane.
///
/// Dereferences to [`PlasmOssHostState`] so existing handlers keep using `st.engine`, `st.sessions`, …
/// SaaS fields are accessed only via dedicated getters (or `st.saas.as_ref()`) to keep the seam clear.
#[derive(Clone)]
pub struct PlasmHostState {
    pub oss: PlasmOssHostState,
    /// Injected in the `plasm` + `plasm-saas` composition; [`None`] for OSS-only HTTP/execute.
    pub saas: Option<PlasmSaaSHostExtension>,
}

impl Deref for PlasmHostState {
    type Target = PlasmOssHostState;

    fn deref(&self) -> &Self::Target {
        &self.oss
    }
}

impl PlasmHostState {
    // --- SaaS / control-plane (None when `self.saas` is unset) ---

    pub fn mcp_config_repository(&self) -> Option<&Arc<McpConfigRepository>> {
        self.saas.as_ref()?.mcp_config_repository.as_ref()
    }

    pub fn mcp_transport_auth(&self) -> Option<&Arc<dyn McpTransportAuth>> {
        self.saas.as_ref()?.mcp_transport_auth.as_ref()
    }

    pub fn auth_storage(&self) -> Option<&Arc<dyn AuthStorage>> {
        self.oss.auth_storage.as_ref()
    }

    pub fn auth_framework(&self) -> Option<&Arc<tokio::sync::Mutex<AuthFramework>>> {
        self.saas.as_ref()?.auth_framework.as_ref()
    }

    /// OAuth account-linking catalog when outbound OAuth is wired on [`PlasmOssHostState`].
    pub fn oauth_link_catalog(&self) -> Option<&Arc<OauthLinkCatalog>> {
        self.oss.oauth_link_catalog.as_ref()
    }

    pub fn tenant_binding(&self) -> Option<&Arc<TenantBindingStore>> {
        self.saas.as_ref()?.tenant_binding.as_ref()
    }

    pub fn incoming_auth_device(&self) -> &IncomingAuthDeviceStore {
        &self.oss.incoming_auth_device
    }

    /// Hosted KV + catalog outbound resolver; absent when not wired.
    pub fn outbound_secret_provider(&self) -> Option<&Arc<dyn SecretProvider>> {
        self.oss.outbound_secret_provider.as_ref()
    }

    /// Typed-discovery embedding lookup when [`PlasmOssHostState::discovery_embedding`] is wired.
    pub fn discovery_embedding_store(&self) -> Option<Arc<dyn CatalogEmbeddingStore>> {
        self.oss
            .discovery_embedding
            .clone()
            .map(|r| r as Arc<dyn CatalogEmbeddingStore>)
    }

    pub fn discovery_index_cache(&self) -> &CatalogIndexCache {
        &self.oss.discovery_index_cache
    }

    #[cfg(feature = "local-embeddings")]
    pub fn discovery_embedder(&self) -> Arc<plasm_discovery::BlockingEmbedder> {
        self.oss.discovery_embedder.clone()
    }

    pub fn workflows(&self) -> &crate::workflow_registry::WorkflowRegistry {
        &self.oss.workflows
    }

    /// Dedicated large-stack pool for live plan execution (see [`crate::live_plan_run_worker`]).
    pub fn live_plan_pool(&self) -> Arc<crate::live_plan_run_worker::LivePlanRunPool> {
        Arc::clone(&self.oss.live_plan_pool)
    }

    /// Stash HTTP User-Agent for an MCP transport session (`mcp-session-id`).
    pub fn record_mcp_http_user_agent(&self, transport_key: &str, user_agent: String) {
        if user_agent.trim().is_empty() {
            return;
        }
        self.oss
            .mcp_http_user_agents
            .insert(transport_key.to_string(), user_agent);
    }

    pub fn mcp_http_user_agent(&self, transport_key: &str) -> Option<String> {
        self.oss
            .mcp_http_user_agents
            .get(transport_key)
            .map(|entry| entry.value().clone())
    }

    /// Outbound HTTP credentials: [`PlasmOssHostState::outbound_secret_provider`] when wired; otherwise [`EnvSecretProvider`].
    pub fn effective_outbound_secret_provider(&self) -> Arc<dyn SecretProvider> {
        if let Some(p) = self.oss.outbound_secret_provider.as_ref() {
            return p.clone();
        }
        Arc::new(EnvSecretProvider) as Arc<dyn SecretProvider>
    }

    /// Reverse lookup: execute `(prompt_hash, session_id)` → logical session id (MCP trace key).
    pub async fn logical_session_id_for_execute_binding(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<Uuid> {
        self.oss
            .logical_execute_bindings
            .find_by_execute(prompt_hash, session_id)
            .await
    }

    /// Local execute row, then Redis-backed rehydrate when configured.
    pub async fn get_execute_session(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<Arc<ExecuteSession>> {
        if let Some(sess) = self.sessions.get_by_strs(prompt_hash, session_id).await {
            let reg = self.catalog.snapshot();
            let pins =
                crate::execute_session_rehydrate::RegistryCatalogPins::from_execute_session(&sess);
            if crate::execute_session_rehydrate::registry_pins_match_live(reg.as_ref(), &pins)
                .is_ok()
            {
                use crate::mcp_transport_store::execute_session_registry::MergeLiveOutcome;
                match self
                    .execute_session_registry
                    .merge_into_live_session(&sess, prompt_hash, session_id)
                    .await
                {
                    MergeLiveOutcome::Merged => return Some(sess),
                    MergeLiveOutcome::NeedsRehydrate => {
                        tracing::info!(
                            target: "plasm_agent::execute_session",
                            prompt_hash = %prompt_hash,
                            session_id = %session_id,
                            hot_revision = sess.domain_revision,
                            "hot execute session behind durable exposure; rehydrating"
                        );
                        // Drop hot only — durable descriptor is the source of truth.
                        self.sessions.remove_by_strs(prompt_hash, session_id).await;
                    }
                }
            } else {
                tracing::info!(
                    target: "plasm_agent::execute_session",
                    prompt_hash = %prompt_hash,
                    session_id = %session_id,
                    "in-memory execute session stale (catalog rotation); discarding"
                );
                self.discard_persisted_execute_row(prompt_hash, session_id)
                    .await;
                return None;
            }
        }
        self.rehydrate_execute_session_from_durable(prompt_hash, session_id)
            .await
    }

    async fn rehydrate_execute_session_from_durable(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<Arc<ExecuteSession>> {
        let desc = self
            .execute_session_registry
            .load(prompt_hash, session_id)
            .await?;
        match crate::execute_session_rehydrate::rehydrate_execute_session(self, &desc).await {
            Ok(session) => {
                crate::metrics::record_execute_rehydrate("ok", "");
                session.bind_operation_wire(session_id);
                let reuse_key: SessionReuseKey = desc.reuse_key.into();
                self.sessions
                    .insert_rehydrated(
                        reuse_key,
                        desc.prompt_hash.clone(),
                        desc.session_id.clone(),
                        session,
                    )
                    .await;
                self.sessions.get_by_strs(prompt_hash, session_id).await
            }
            Err(err) => {
                crate::metrics::record_execute_rehydrate("error", rehydrate_error_kind(&err));
                if crate::execute_session_rehydrate::should_discard_persisted_execute_on_rehydrate_error(
                    &err,
                ) {
                    self.discard_persisted_execute_row(prompt_hash, session_id)
                        .await;
                }
                tracing::warn!(
                    target: "plasm_agent::execute_session",
                    prompt_hash = %prompt_hash,
                    session_id = %session_id,
                    error = %err,
                    "execute session rehydrate failed"
                );
                None
            }
        }
    }

    /// Remove Redis descriptor, logical binding, and in-memory row for one execute session.
    pub async fn discard_persisted_execute_row(&self, prompt_hash: &str, session_id: &str) {
        self.execute_session_registry
            .delete(prompt_hash, session_id)
            .await;
        self.logical_execute_bindings
            .delete_for_execute(prompt_hash, session_id)
            .await;
        self.sessions.remove_by_strs(prompt_hash, session_id).await;
    }

    /// Purge cross-pod execute transport after plugin catalog reload (Redis + local caches).
    pub async fn purge_persisted_execute_state(&self) -> (u64, u64) {
        self.sessions.purge_all().await;
        let session_keys = self.execute_session_registry.purge_redis().await;
        let logical_keys = self.logical_execute_bindings.purge_redis_and_local().await;
        let ledger_keys = self.logical_symbol_ledgers.purge_durable_layers().await;
        (session_keys, logical_keys.saturating_add(ledger_keys))
    }

    /// Insert a new execute session and mirror descriptor to Redis when configured.
    pub async fn store_execute_session(
        &self,
        reuse_key: SessionReuseKey,
        prompt_hash: String,
        session_id: String,
        session: ExecuteSession,
    ) {
        session.bind_operation_wire(&session_id);
        self.execute_session_registry
            .persist(&session, &session_id, &reuse_key)
            .await;
        self.sessions
            .insert(reuse_key, prompt_hash, session_id, session)
            .await;
    }

    /// Replace in-memory session payload and refresh Redis descriptor (reuse key preserved when present).
    ///
    /// Durable persist is required when a backend is configured — never advance hot alone (split-brain).
    pub async fn replace_execute_session(
        &self,
        prompt_hash: &str,
        session_id: &str,
        session: ExecuteSession,
    ) -> Result<(), crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError>
    {
        use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError;

        // Parse path ids before durable write — never advance durable without hot.
        let ph: PromptHashHex = prompt_hash
            .parse()
            .map_err(|_| ExecuteSessionPersistError::SessionUnavailable)?;
        let sid: ExecuteSessionId = session_id
            .parse()
            .map_err(|_| ExecuteSessionPersistError::SessionUnavailable)?;
        let reuse_key = self
            .sessions
            .reuse_key_for_execute_pair(prompt_hash, session_id)
            .await;
        self.execute_session_registry
            .persist_or_update(&session, session_id, reuse_key.as_ref())
            .await?;
        self.sessions.replace_session(&ph, &sid, session).await;
        Ok(())
    }
}

fn rehydrate_error_kind(err: &crate::execute_session_rehydrate::RehydrateError) -> &'static str {
    use crate::execute_session_rehydrate::RehydrateError;
    match err {
        RehydrateError::UnknownEntry(_) => "unknown_entry",
        RehydrateError::CatalogHashMismatch { .. } => "catalog_hash_mismatch",
        RehydrateError::DescriptorExpired => "descriptor_expired",
        RehydrateError::EntityCatalogPairingMismatch { .. } => "entity_catalog_pairing_mismatch",
        RehydrateError::Discovery(_) => "discovery",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use plasm_core::discovery::{CgsCatalog, InMemoryCgsRegistry};
    use plasm_core::loader::load_schema_dir;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

    use super::*;
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::http_execute::execute_session_create_response_inner;
    use crate::http_execute::CreateExecuteSessionBody;
    use crate::mcp_transport_store::ExecuteSessionRegistry;
    use crate::run_artifacts::RunArtifactStore;
    use plasm_core::AuthScheme;

    fn test_host_state_from_registry(reg: InMemoryCgsRegistry) -> PlasmHostState {
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    fn test_host_state() -> PlasmHostState {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )]);
        test_host_state_from_registry(reg)
    }

    fn test_host_state_with_shared(
        registry: Arc<InMemoryCgsRegistry>,
        execute_session_registry: ExecuteSessionRegistry,
    ) -> PlasmHostState {
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let mut st = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry,
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        st.oss.execute_session_registry = execute_session_registry;
        st
    }

    fn fibery_shaped_registry() -> InMemoryCgsRegistry {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let mut cgs = load_schema_dir(&dir).expect("overshow_tools");
        cgs.http_backend = "https://YOUR_ACCOUNT.fibery.io".to_string();
        InMemoryCgsRegistry::from_pairs(vec![(
            "fibery".into(),
            "Fibery".into(),
            vec!["Profile".into()],
            Arc::new(cgs),
        )])
    }

    fn rotated_overshow_registry() -> InMemoryCgsRegistry {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let mut cgs = load_schema_dir(&dir).expect("overshow_tools");
        cgs.http_backend.push_str("/catalog-reload-test");
        let cgs = cgs.fresh_catalog_digest();
        InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            Arc::new(cgs),
        )])
    }

    #[tokio::test]
    async fn get_execute_session_none_after_in_memory_catalog_stale() {
        let st = test_host_state();
        let created = execute_session_create_response_inner(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
            true,
            None,
            None,
            None,
            false,
        )
        .await
        .expect("open session");

        assert!(st
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .is_some());

        st.catalog
            .publish_catalog(Arc::new(rotated_overshow_registry()));

        assert!(st
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .is_none());
        assert!(st
            .sessions
            .get_by_strs(&created.prompt_hash, &created.session)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn get_execute_session_survives_outbound_hosted_auth_patch() {
        let st = test_host_state();
        let mut hosted = std::collections::HashMap::new();
        hosted.insert("overshow".to_string(), "tenant/test/oauth".to_string());
        let created = execute_session_create_response_inner(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
            false,
            Some(&hosted),
            None,
            None,
            false,
        )
        .await
        .expect("open session with tenant hosted_kv patch");

        let reg = st.catalog.snapshot();
        let session = st
            .sessions
            .get_by_strs(&created.prompt_hash, &created.session)
            .await
            .expect("session row");
        let effective = session
            .contexts_by_entry
            .get("overshow")
            .expect("overshow ctx")
            .cgs
            .effective_catalog_cgs_hash_hex();
        let registry_base = reg
            .load_context("overshow")
            .expect("overshow")
            .cgs
            .catalog_cgs_hash_hex();
        assert_ne!(
            effective, registry_base,
            "tenant patch must change effective digest vs registry base"
        );
        assert!(matches!(
            session.contexts_by_entry["overshow"].cgs.auth,
            Some(AuthScheme::BearerToken { .. })
        ));

        assert!(
            st.get_execute_session(&created.prompt_hash, &created.session)
                .await
                .is_some(),
            "get_execute_session must not false-discard tenant-patched session"
        );
    }

    #[tokio::test]
    async fn get_execute_session_survives_resolved_http_backend_patch() {
        let st = test_host_state_from_registry(fibery_shaped_registry());
        let mut bindings = std::collections::HashMap::new();
        bindings.insert(
            "fibery".to_string(),
            crate::session_bindings::repl_session_binding_map(
                "fibery",
                crate::http_backend::ReplHttpOverride::from_engine_base("https://acme.fibery.io")
                    .expect("origin"),
            )
            .expect("bindings"),
        );
        let created = execute_session_create_response_inner(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "fibery".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
            false,
            None,
            Some(&bindings),
            None,
            false,
        )
        .await
        .expect("open fibery session with resolved backend");

        let reg = st.catalog.snapshot();
        let session = st
            .sessions
            .get_by_strs(&created.prompt_hash, &created.session)
            .await
            .expect("session row");
        let effective = session.contexts_by_entry["fibery"]
            .cgs
            .effective_catalog_cgs_hash_hex();
        let registry_base = reg
            .load_context("fibery")
            .expect("fibery")
            .cgs
            .catalog_cgs_hash_hex();
        assert_ne!(effective, registry_base);
        assert_eq!(
            session.contexts_by_entry["fibery"].cgs.http_backend,
            "https://acme.fibery.io"
        );

        assert!(st
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .is_some());
    }

    #[tokio::test]
    async fn tenant_patch_alone_does_not_trigger_catalog_stale_discard() {
        let st = test_host_state();
        let mut hosted = std::collections::HashMap::new();
        hosted.insert("overshow".to_string(), "tenant/test/oauth".to_string());
        let created = execute_session_create_response_inner(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
            false,
            Some(&hosted),
            None,
            None,
            false,
        )
        .await
        .expect("open session");

        // Registry unchanged — only tenant materialization differs from live effective hash.
        assert!(st
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .is_some());
    }

    #[tokio::test]
    async fn cross_pod_rehydrate_after_tenant_materialization_patch() {
        let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )]));
        let host_a = test_host_state_with_shared(registry.clone(), execute_registry.clone());
        let host_b = test_host_state_with_shared(registry, execute_registry);

        let mut hosted = std::collections::HashMap::new();
        hosted.insert("overshow".to_string(), "tenant/test/oauth".to_string());
        let created = execute_session_create_response_inner(
            &host_a,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
            false,
            Some(&hosted),
            None,
            None,
            false,
        )
        .await
        .expect("open on host A");

        assert!(host_a
            .sessions
            .get_by_strs(&created.prompt_hash, &created.session)
            .await
            .is_some());
        host_a.sessions.purge_all().await;

        let rehydrated = host_b
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .expect("host B rehydrate");
        assert!(matches!(
            &rehydrated.contexts_by_entry["overshow"].cgs.auth,
            Some(AuthScheme::BearerToken {
                hosted_kv: Some(kv),
                ..
            }) if kv == "tenant/test/oauth"
        ));
    }
}
