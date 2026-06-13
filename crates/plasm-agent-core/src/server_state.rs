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
use crate::mcp_transport_store::{ExecuteSessionRegistry, LogicalExecuteBindingRegistry};
use crate::oauth_link_catalog::OauthLinkCatalog;
use crate::operation_progress::OperationProgressHub;
use crate::run_artifacts::RunArtifactStore;
use crate::session_graph_persistence::SessionGraphPersistence;
use crate::session_identity::LogicalSessionRegistry;
use crate::tenant_binding::TenantBindingStore;
use crate::trace_hub::{TraceHub, TraceHubConfig};
use crate::trace_sink_emit::TraceIngestClient;
use auth_framework::storage::AuthStorage;
use auth_framework::AuthFramework;
use plasm_discovery::embedding_store::CatalogEmbeddingStore;
use plasm_discovery::CatalogIndexCache;
use plasm_plugin_host::PluginManager;
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
    /// Durable execute-session descriptors for cross-pod rehydration (`PLASM_MCP_TRANSPORT_REDIS_URL`).
    pub execute_session_registry: ExecuteSessionRegistry,
    /// Stored execute run snapshots (`GET .../artifacts/:run_id`, MCP `resources/read`). See [`crate::run_artifacts`].
    pub run_artifacts: Arc<RunArtifactStore>,
    /// Optional object-store-backed delta/snapshot persistence for session graph state.
    pub session_graph_persistence: Option<Arc<SessionGraphPersistence>>,
    /// Optional compile-plugin manager (`--compile-plugin`); new execute sessions pin current generation.
    pub plugin_manager: Option<Arc<PluginManager>>,
    /// When set, HTTP routes run [`crate::incoming_auth::incoming_auth_http_middleware`].
    pub incoming_auth: Option<Arc<IncomingAuthVerifier>>,
    /// CLI device-login marker ([`crate::incoming_auth_device`] sessions live in auth-framework KV).
    pub incoming_auth_device: Arc<IncomingAuthDeviceStore>,
    /// MCP transport session traces (demo/debug; in-memory).
    pub trace_hub: Arc<TraceHub>,
    /// Queued MCP `notifications/plasm/op` payloads (drained by MCP session reporter).
    pub op_progress_hub: Arc<OperationProgressHub>,
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
            return Some(sess);
        }
        let desc = self
            .execute_session_registry
            .load(prompt_hash, session_id)
            .await?;
        match crate::execute_session_rehydrate::rehydrate_execute_session(self, &desc).await {
            Ok(session) => {
                crate::metrics::record_execute_rehydrate("ok", "");
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

    /// Insert a new execute session and mirror descriptor to Redis when configured.
    pub async fn store_execute_session(
        &self,
        reuse_key: SessionReuseKey,
        prompt_hash: String,
        session_id: String,
        session: ExecuteSession,
    ) {
        self.execute_session_registry
            .persist(&session, &session_id, &reuse_key)
            .await;
        self.sessions
            .insert(reuse_key, prompt_hash, session_id, session)
            .await;
    }

    /// Replace in-memory session payload and refresh Redis descriptor (reuse key preserved when present).
    pub async fn replace_execute_session(
        &self,
        prompt_hash: &str,
        session_id: &str,
        session: ExecuteSession,
    ) {
        self.execute_session_registry
            .persist_or_update(&session, session_id)
            .await;
        if let (Ok(ph), Ok(sid)) = (
            prompt_hash.parse::<PromptHashHex>(),
            session_id.parse::<ExecuteSessionId>(),
        ) {
            self.sessions.replace_session(&ph, &sid, session).await;
        }
    }
}

fn rehydrate_error_kind(err: &crate::execute_session_rehydrate::RehydrateError) -> &'static str {
    use crate::execute_session_rehydrate::RehydrateError;
    match err {
        RehydrateError::UnknownEntry(_) => "unknown_entry",
        RehydrateError::CatalogHashMismatch { .. } => "catalog_hash_mismatch",
        RehydrateError::DescriptorExpired => "descriptor_expired",
        RehydrateError::Discovery(_) => "discovery",
        RehydrateError::PluginGenerationUnavailable { .. } => "plugin_generation_unavailable",
    }
}
