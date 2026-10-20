//! Request-scoped host facts, independent of the MCP transport implementation.

use crate::{mcp_client_info::McpClientInfo, mcp_stream_identity::McpTransportIdentity};
use rmcp::{service::RequestContext, RoleServer};

pub(crate) struct McpRequest {
    pub transport_key: String,
    pub client: McpClientInfo,
    pub identity: Option<McpTransportIdentity>,
}

impl McpRequest {
    pub fn from_context(context: &RequestContext<RoleServer>) -> Self {
        let parts = context.extensions.get::<http::request::Parts>();
        let transport_key = parts
            .and_then(|p| p.headers.get("mcp-session-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("stateless-{}", uuid::Uuid::new_v4()));
        let client = match (context.client_info(), context.client_capabilities()) {
            (Some(info), Some(capabilities)) => McpClientInfo::from_initialize(
                rmcp::model::InitializeRequestParams::new(capabilities, info)
                    .with_protocol_version(context.protocol_version().unwrap_or_default()),
            ),
            _ => McpClientInfo::Default,
        };
        Self {
            transport_key,
            client,
            identity: parts
                .and_then(|p| p.extensions.get::<McpTransportIdentity>())
                .cloned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolFailure {
    #[error("{0}")]
    Rejected(String),
    #[error("Invalid arguments for {tool}: {detail}")]
    InvalidArguments { tool: String, detail: String },
    #[error("Unknown tool: {0}")]
    UnknownTool(String),
    #[error(transparent)]
    Cause(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl ToolFailure {
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }
    pub fn invalid_arguments(tool: impl Into<String>, detail: Option<String>) -> Self {
        Self::InvalidArguments {
            tool: tool.into(),
            detail: detail.unwrap_or_default(),
        }
    }
    pub fn unknown_tool(tool: impl Into<String>) -> Self {
        Self::UnknownTool(tool.into())
    }
    pub fn cause(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Cause(Box::new(error))
    }
    pub fn into_result(self) -> rmcp::model::CallToolResult {
        rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(self.to_string())])
    }
}

pub(crate) fn tool_error_result(error: ToolFailure) -> rmcp::model::CallToolResult {
    error.into_result()
}

#[derive(Clone)]
pub(crate) struct AuthenticationState {
    pub provider: std::sync::Arc<crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider>,
    pub session_store: Option<std::sync::Arc<crate::mcp_transport_store::RedisMcpSessionStore>>,
}

pub(crate) async fn authenticate(
    axum::extract::State(state): axum::extract::State<AuthenticationState>,
    mut request: http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let provider = &state.provider;
    let token = match request.headers().get(http::header::AUTHORIZATION) {
        None => Some(""),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
            .map(|(_, value)| value),
    };
    if let Some(token) = token {
        if let Ok(identity) = provider.verify_token(token.to_owned()).await {
            if let (Some(store), Some(id)) = (
                &state.session_store,
                request
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok()),
            ) {
                if let Err(error) = store.touch(id).await {
                    tracing::warn!(%error, "MCP session TTL refresh failed");
                    return (
                        http::StatusCode::SERVICE_UNAVAILABLE,
                        "MCP session store unavailable",
                    )
                        .into_response();
                }
            }
            request.extensions_mut().insert(identity);
            return next.run(request).await;
        }
    }
    let mut response =
        (http::StatusCode::UNAUTHORIZED, "MCP authorization required").into_response();
    if let Ok(challenge) = provider.bearer_challenge(true) {
        response
            .headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, challenge);
    }
    response
}
