//! Native rmcp Streamable HTTP service; Plasm owns authentication and host policy.
use super::{mcp_http_dns_rebinding, mcp_http_user_agent, PlasmMcpHandler};
use crate::{
    mcp_transport_store::{PlasmTransportRedisStore, RedisMcpSessionStore},
    server_state::PlasmHostState,
};
use axum::{middleware, routing::get, Router};
use rmcp::model::{Implementation, InitializeResult, ProtocolVersion, ServerCapabilities};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("MCP listener failed")]
    Io(#[from] std::io::Error),
    #[error("MCP session store unavailable")]
    Redis(#[from] redis::RedisError),
}

pub(crate) fn mcp_initialize_result() -> InitializeResult {
    let mut result = InitializeResult::new(
        ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build(),
    );
    result.protocol_version = ProtocolVersion::V_2025_11_25;
    result.server_info = Implementation::new("plasm", crate::release_version::RELEASE_VERSION);
    result.server_info.title = Some("Plasm agent".into());
    result.server_info.description = Some(
        "**`session_mode: \"new\"`** once per workflow with current **`intent`**, then **`\"extend\"`** + **`logical_session_ref`** with the current need. **`plasm`** (plan) and **`plasm_run`** (execute) reuse the same ref. **`intent`** extends a lossless provenance chain per turn; current intent drives retrieval, ancestry informs judgment. Intent does not select the session.".into(),
    );
    result.instructions = Some(plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW.into());
    result
}

pub fn plasm_mcp_stateless_enabled() -> bool {
    matches!(
        std::env::var("PLASM_MCP_STATELESS")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

#[derive(Clone, Copy, Debug)]
pub enum McpHttpTransport {
    StreamableHttp,
    Stateless,
}

pub async fn build_mcp_router_for_merge(
    plasm: Arc<PlasmHostState>,
) -> Result<Router, McpServerError> {
    let transport = if plasm_mcp_stateless_enabled() {
        McpHttpTransport::Stateless
    } else {
        McpHttpTransport::StreamableHttp
    };
    build_mcp_router_for_transport(plasm, transport).await
}

pub async fn build_mcp_router_for_transport(
    plasm: Arc<PlasmHostState>,
    transport: McpHttpTransport,
) -> Result<Router, McpServerError> {
    let mut handler = PlasmMcpHandler::new(plasm.clone());
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = matches!(transport, McpHttpTransport::StreamableHttp);
    config.stateless_protocol_metadata_required = true;
    config.json_response = true;
    // The Plasm middleware retains the configured hosted/loopback Host and Origin policy.
    config.allowed_hosts.clear();
    let mut session_store = None;
    if let Some(backend) = &plasm.redis_backend {
        backend.ping().await?;
        handler =
            handler.with_transport_redis(Arc::new(PlasmTransportRedisStore::new(backend.clone())));
        let store = Arc::new(RedisMcpSessionStore::new(backend.clone()));
        config.session_store = Some(store.clone());
        session_store = Some(store);
    }
    let peers = handler.peers.clone();
    let sessions = handler.session_states.clone();
    let manager = Arc::new(LocalSessionManager::default());
    super::teaching_prompt_reporter::spawn_mcp_teaching_prompt_session_reporter(
        peers,
        plasm.clone(),
        sessions,
    );
    let service = StreamableHttpService::new(move || Ok(handler.clone()), manager, config);
    let provider = Arc::new(crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::new(
        plasm.clone(),
    ));
    let mcp =
        Router::new()
            .nest_service("/mcp", service)
            .route_layer(middleware::from_fn_with_state(
                super::request::AuthenticationState {
                    provider: provider.clone(),
                    session_store,
                },
                super::request::authenticate,
            ));
    Ok(mcp
        .merge(crate::mcp_oauth_http::router(provider))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn(
            mcp_http_dns_rebinding::reject_dns_rebinding,
        ))
        .layer(middleware::from_fn_with_state(
            plasm,
            mcp_http_user_agent::capture_mcp_http_user_agent,
        )))
}

pub async fn run_mcp_server(
    host: &str,
    port: u16,
    plasm: Arc<PlasmHostState>,
) -> Result<(), McpServerError> {
    let router = build_mcp_router_for_merge(plasm).await?;
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
