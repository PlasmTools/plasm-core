//! MCP server initialize metadata, HyperServer construction, and merge/router helpers.

use std::sync::Arc;

use plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::event_store::InMemoryEventStore;
use rust_mcp_sdk::mcp_server::hyper_server;
use rust_mcp_sdk::mcp_server::{HyperServer, HyperServerOptions, ToMcpServerHandler};
use rust_mcp_sdk::schema::SdkError;
use rust_mcp_sdk::schema::{
    Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
    ServerCapabilitiesResources, ServerCapabilitiesTools,
};
use rust_mcp_sdk::session_store::SessionStore;

use crate::mcp_transport_store::{
    PlasmTransportRedisStore, RedisSessionStore, SessionRuntimeFactory,
};
use crate::server_state::PlasmHostState;

use super::stateless;
use super::teaching_prompt_reporter::spawn_mcp_teaching_prompt_session_reporter;
use super::{mcp_http_dns_rebinding, mcp_http_user_agent, PlasmMcpHandler};

/// Server metadata for stateless MCP (`server/discover`, per-request `_meta`).
pub(crate) fn mcp_stateless_server_details() -> InitializeResult {
    let mut init = mcp_initialize_result();
    init.protocol_version = stateless::STATELESS_PROTOCOL_VERSION.into();
    init
}

pub(crate) fn mcp_initialize_result() -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: "plasm".into(),
            version: crate::release_version::RELEASE_VERSION.into(),
            title: Some("Plasm agent".into()),
            description: Some(
                "**`session_mode: \"new\"`** once per workflow, then **`\"extend\"`** + **`logical_session_ref`**. **`plasm`** (plan) and **`plasm_run`** (execute) reuse the same ref. **`intent`** accumulates per turn — it does not select the session."
                    .into(),
            ),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: None,
                subscribe: Some(false),
            }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: Some(MCP_INITIALIZE_WORKFLOW.to_string()),
        meta: None,
    }
}

/// Whether stateless SEP-2575 transport is active (`PLASM_MCP_STATELESS=1`).
pub fn plasm_mcp_stateless_enabled() -> bool {
    stateless::plasm_mcp_stateless_enabled()
}

/// MCP routes for merging with discovery/execute on one port (stateful SDK or stateless axum).
pub async fn build_mcp_router_for_merge(plasm: Arc<PlasmHostState>) -> SdkResult<axum::Router> {
    if plasm_mcp_stateless_enabled() {
        tracing::info!("MCP transport: stateless (SEP-2575, PLASM_MCP_STATELESS)");
        return Ok(stateless::router(plasm).await);
    }
    let server = build_mcp_hyper_server(Arc::clone(&plasm), "0.0.0.0", 0).await?;
    Ok(mcp_hyper_router(server, plasm))
}

/// Build MCP Streamable HTTP server (not started) for merging with discovery routes on one port.
pub async fn build_mcp_hyper_server_for_merge(
    plasm: Arc<PlasmHostState>,
) -> SdkResult<HyperServer> {
    if plasm_mcp_stateless_enabled() {
        return Err(rust_mcp_sdk::error::McpSdkError::SdkError(
            SdkError::invalid_request().with_message(
                "PLASM_MCP_STATELESS=1: use build_mcp_router_for_merge instead of build_mcp_hyper_server_for_merge",
            ),
        ));
    }
    build_mcp_hyper_server(plasm, "0.0.0.0", 0).await
}

/// MCP HTTP routes with User-Agent capture for artifact-access detection.
pub fn mcp_hyper_router(server: HyperServer, plasm: Arc<PlasmHostState>) -> axum::Router<()> {
    use axum::middleware;
    server
        .into_router()
        .layer(middleware::from_fn(
            mcp_http_dns_rebinding::reject_dns_rebinding,
        ))
        .layer(middleware::from_fn_with_state(
            plasm,
            mcp_http_user_agent::capture_mcp_http_user_agent,
        ))
}

pub(crate) async fn build_mcp_hyper_server(
    plasm: Arc<PlasmHostState>,
    host: &str,
    port: u16,
) -> SdkResult<HyperServer> {
    let mut handler_struct = PlasmMcpHandler::new(Arc::clone(&plasm));
    let session_states = Arc::clone(&handler_struct.session_states);
    let server_details = Arc::new(mcp_initialize_result());

    if let Some(backend) = plasm.redis_backend.as_ref() {
        let plasm_redis = Arc::new(PlasmTransportRedisStore::new(Arc::clone(backend)));
        handler_struct = handler_struct.with_transport_redis(plasm_redis);
    }

    let mcp_handler = handler_struct.to_mcp_server_handler();

    let session_store: Option<Arc<dyn SessionStore>> =
        if let Some(backend) = plasm.redis_backend.as_ref() {
            let store: Arc<RedisSessionStore> = Arc::new(RedisSessionStore::new(
                Arc::clone(backend),
                Arc::new(SessionRuntimeFactory {
                    server_details: Arc::clone(&server_details),
                    handler: mcp_handler.clone(),
                    task_store: None,
                    client_task_store: None,
                    message_observer: None,
                }),
            ));
            store.ping().await.map_err(|e| {
                SdkError::internal_error().with_message(&format!(
                    "PLASM_MCP_TRANSPORT_REDIS_URL configured but Redis ping failed: {e}"
                ))
            })?;
            tracing::info!("MCP transport session store: Redis (multi-replica safe)");
            Some(store)
        } else {
            None
        };

    let auth_provider: Option<Arc<dyn rust_mcp_sdk::auth::AuthProvider>> =
        if plasm.mcp_config_repository().is_some() || plasm.incoming_auth.is_some() {
            Some(Arc::new(
                crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::new(Arc::clone(&plasm)),
            ))
        } else {
            None
        };
    let server = hyper_server::create_server(
        (*server_details).clone(),
        mcp_handler,
        HyperServerOptions {
            host: host.to_string(),
            port,
            event_store: Some(Arc::new(InMemoryEventStore::default())),
            health_endpoint: Some("/health".into()),
            sse_support: false,
            auth: auth_provider,
            session_store,
            ..Default::default()
        },
    );
    spawn_mcp_teaching_prompt_session_reporter(&server, Arc::clone(&plasm), session_states);
    Ok(server)
}

/// Run Streamable HTTP MCP on `host`:`port` (default MCP path `/mcp` from the SDK).
pub async fn run_mcp_server(host: &str, port: u16, plasm: Arc<PlasmHostState>) -> SdkResult<()> {
    if plasm_mcp_stateless_enabled() {
        let router = stateless::router(plasm).await;
        let addr = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| SdkError::internal_error().with_message(&format!("bind {addr}: {e}")))?;
        tracing::info!("MCP stateless HTTP listening on http://{addr}");
        axum::serve(listener, router)
            .await
            .map_err(|e| SdkError::internal_error().with_message(&e.to_string()))?;
        return Ok(());
    }
    let server = build_mcp_hyper_server(plasm, host, port).await?;
    server.start().await
}
