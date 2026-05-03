//! HTTP server mode: discovery ([`crate::http_discovery`]) + execute session ([`crate::http_execute`]).
//!
//! Flow: `POST /v1/discover` → use `entry_id` + `entity` from candidates → `POST /execute` → follow
//! `Location` with `GET` (session JSON) → `POST` the same path with Plasm lines.
//!
//! - `GET /v1/health`, `GET /v1/auth/status` (liveness + capability probe: OSS returns `200` with `open_source: true` when no SaaS extension; hosted builds without `auth_framework` return `503`), `GET /v1/registry`, …, `POST /v1/discover`
//! - `POST /execute` — JSON `{ entry_id, entities, principal? }` → `303` + `Location` only (no body); ids are in the URL (`principal` required when `PLASM_AUTH_RESOLUTION=delegated`)
//! - `GET /execute/:prompt_hash/:session` — `200` + JSON (`prompt`, `entry_id`, `entities`, …)
//! - `POST /execute/:prompt_hash/:session` — `text/plain` or JSON (`lines` array, or top-level array of line strings); `Accept`: json | ndjson | table | toon (**default** when omitted: **toon**, entity rows only; no duration/cache metadata)

use axum::extract::Extension;
use axum::routing::get;
use axum::Router;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_runtime::{ExecutionEngine, ExecutionMode};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use crate::catalog_runtime::CatalogRuntime;
use crate::execute_session::ExecuteSessionStore;
use crate::http_discovery::{discovery_routes_protected, get_auth_status, health_response};
use crate::http_execute::execute_routes;
use crate::http_incoming_context::incoming_context_routes;
use crate::http_oauth_link;
use crate::http_outbound_secrets;
use crate::http_traces::trace_routes;
use crate::incoming_auth::incoming_auth_http_middleware;
use crate::incoming_auth::IncomingAuthVerifier;
use crate::local_trace_archive::LocalTraceArchive;
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::{CatalogBootstrap, PlasmHostState, PlasmOssHostState};
use crate::session_graph_persistence::SessionGraphPersistence;
use crate::session_identity::LogicalSessionRegistry;
use crate::trace_hub::{TraceHubBuilder, TraceHubConfig};
use crate::trace_sink_emit::{EnvTraceIngestClient, TraceIngestClient};
use plasm_otel::tower_http_trace_parent_span;
use plasm_plugin_host::PluginManager;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Inputs for [`build_plasm_host_state`] (keeps the surface under clippy’s argument limit).
pub struct PlasmHostBootstrap {
    pub engine: ExecutionEngine,
    pub mode: ExecutionMode,
    pub registry: Arc<InMemoryCgsRegistry>,
    pub catalog_bootstrap: CatalogBootstrap,
    pub plugin_manager: Option<Arc<PluginManager>>,
    pub incoming_auth: Option<Arc<IncomingAuthVerifier>>,
    pub run_artifacts: Arc<RunArtifactStore>,
    pub session_graph_persistence: Option<Arc<SessionGraphPersistence>>,
    /// When true, [`LocalTraceArchive::from_env_or_oss_default`] uses `~/.plasm/local` if `PLASM_TRACE_ARCHIVE_DIR` is unset.
    pub oss_local_filesystem_defaults: bool,
}

/// Build shared state for HTTP + MCP (registry, engine, session store).
pub fn build_plasm_host_state(bootstrap: PlasmHostBootstrap) -> PlasmHostState {
    let PlasmHostBootstrap {
        engine,
        mode,
        registry,
        catalog_bootstrap,
        plugin_manager,
        incoming_auth,
        run_artifacts,
        session_graph_persistence,
        oss_local_filesystem_defaults,
    } = bootstrap;
    let sessions = Arc::new(ExecuteSessionStore::new(
        run_artifacts.clone(),
        session_graph_persistence.clone(),
    ));
    let catalog = CatalogRuntime::new(registry, catalog_bootstrap);
    let trace_ingest: Arc<dyn TraceIngestClient> = Arc::new(EnvTraceIngestClient);
    let local_trace_archive =
        match LocalTraceArchive::from_env_or_oss_default(oss_local_filesystem_defaults) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(
                    target: "plasm_agent::http",
                    error = %e,
                    "PLASM_TRACE_ARCHIVE_DIR: invalid path (disabling local trace archive)"
                );
                None
            }
        };
    let trace_hub_requested = TraceHubConfig::from_env();
    let trace_hub = Arc::new(
        TraceHubBuilder::from_config(trace_hub_requested)
            .build(Some(trace_ingest.clone()), local_trace_archive.clone()),
    );
    let trace_hub_config = TraceHubConfig {
        bounds: trace_hub.bounds(),
    };
    PlasmHostState {
        oss: PlasmOssHostState {
            engine: Arc::new(engine),
            mode,
            catalog,
            sessions,
            logical_sessions: Arc::new(LogicalSessionRegistry::new()),
            logical_execute_bindings: Arc::new(RwLock::new(HashMap::new())),
            run_artifacts,
            session_graph_persistence,
            plugin_manager,
            incoming_auth,
            trace_hub,
            trace_hub_config,
            trace_ingest,
            local_trace_archive,
            trace_sink_read_base_url: std::env::var("PLASM_TRACE_SINK_READ_URL")
                .ok()
                .or_else(|| std::env::var("PLASM_TRACE_SINK_URL").ok())
                .map(|s| s.trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
            auth_storage: None,
            oauth_link_catalog: None,
            outbound_secret_provider: None,
        },
        saas: None,
    }
}

/// Public liveness and auth status routes (kept out of the traced subtree to avoid log noise on probes).
pub fn health_public_routes() -> Router {
    Router::new()
        .route("/v1/health", get(health_response))
        .route("/v1/auth/status", get(get_auth_status))
}

/// The traced OSS tool/discovery/execute surface (and related `/v1/*` helpers) under incoming-auth
/// middleware.
///
/// The SaaS `/internal/*` stack is mounted *before* this in `plasm-saas` so control-plane clients can
/// call the agent without going through the interactive shell auth middleware.
pub fn oss_traced_routes() -> Router {
    // NOTE: this mirrors the *tail* of the pre-split `plasm_agent::http::discovery_execute_router`:
    // SaaS pre-routes, then a protected bundle under incoming-auth, then a trace layer around both.
    //
    // `plasm-saas` supplies the pre-routes; this module supplies the post-routes.
    Router::new().merge(
        Router::new()
            .merge(discovery_routes_protected())
            .merge(incoming_context_routes())
            .merge(trace_routes())
            .merge(execute_routes())
            .layer(axum::middleware::from_fn(incoming_auth_http_middleware)),
    )
}

/// Discovery + execute routes for a pre-built [`PlasmHostState`].
///
/// State is injected with [`Extension`] so this stays `Router<()>` and works with [`axum::serve`].
pub fn discovery_execute_router(state: PlasmHostState) -> Router {
    // This is the OSS surface only; hosted deployments should use `plasm_saas::http::plasm_mcp_http_app`.
    //
    // Layer order: Extension → routes. Incoming-auth middleware runs on the protected subtree only;
    // `/v1/health` and `/v1/auth/status` stay public.
    //
    // `TraceLayer` is **not** applied to `/v1/health` or `/v1/auth/status` — kube probes and uptime
    // checks would flood logs at `RUST_LOG=tower_http=debug` (or OTLP export noise).
    let health_public = health_public_routes();

    let mut pre_internal = Router::new()
        .merge(http_oauth_link::oauth_link_routes())
        .merge(http_outbound_secrets::outbound_secrets_routes());
    if state.mcp_config_repository().is_some() {
        pre_internal = pre_internal.merge(crate::http_mcp_config::mcp_config_routes());
    }

    let traced = pre_internal
        .merge(oss_traced_routes())
        .layer(TraceLayer::new_for_http().make_span_with(tower_http_trace_parent_span));

    Router::new()
        .merge(health_public)
        .merge(traced)
        .layer(Extension(state))
}

/// Bind `0.0.0.0:port` and serve discovery + execute (used by `--http` alone or with `--mcp`).
pub async fn serve_http_listener(
    state: PlasmHostState,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = discovery_execute_router(state);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("plasm-agent HTTP listening on http://{addr}");
    eprintln!("plasm-agent HTTP mode: http://127.0.0.1:{port}");
    eprintln!(
        "  GET  /v1/health   GET /v1/auth/status   GET /v1/registry   GET /v1/registry/:entry_id   GET /v1/registry/:entry_id/tool-model   GET /v1/incoming-auth/context   POST /v1/discover"
    );
    eprintln!(
        "  GET  /oauth/link/callback   POST /internal/oauth-link/v1/start   POST /internal/outbound-secrets/v1/put   POST /internal/outbound-secrets/v1/delete (when outbound OAuth KV is configured)"
    );
    eprintln!(
        "  When DATABASE_URL / PLASM_MCP_CONFIG_DATABASE_URL is set: POST /internal/mcp-config/v1/upsert (+ MCP API key routes) with X-Plasm-Control-Plane-Secret — same contract as hosted control plane"
    );
    eprintln!(
        "  POST /execute — {{ entry_id, entities }} → 303 Location only → GET that URL for session JSON + DOMAIN prompt"
    );
    eprintln!(
        "  POST /execute/:prompt_hash/:session — text/plain or JSON lines; default Accept: text/toon (results only); also json | x-ndjson | text/plain"
    );
    eprintln!(
        "  GET  /execute/:prompt_hash/:session/artifacts/:run_id — stored run artifact bytes (served from active session memory or durable storage)"
    );
    eprintln!(
        "  GET  /execute/:prompt_hash/:session/plans/:plan_id — archived serialized program plan IR / evaluation artifact (or /plans/by-index/:n)"
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
