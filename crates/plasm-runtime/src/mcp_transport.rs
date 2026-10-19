//! Host-owned outbound MCP connections. Tool calls are never replayed.

use rmcp::{
    model::{CallToolResponse, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion},
    service::{ClientInitializeError, RunningService, ServiceError},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ClientLifecycleMode, ClientServiceExt, RoleClient,
};
use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};
use thiserror::Error;

pub use rmcp::model::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
};

#[derive(Debug, Error)]
pub enum McpProtocolError {
    #[error(transparent)]
    Initialize(#[from] ClientInitializeError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Shutdown(#[from] tokio::task::JoinError),
}

/// Protocol lifecycle is explicit; failure never selects another protocol.
#[derive(Clone, Copy, Debug)]
pub enum McpHttpProtocol {
    Initialize2025,
    Discover2026,
}

pub struct McpHttpConfig {
    pub url: url::Url,
    pub headers: HashMap<String, String>,
    pub deadline: Duration,
}

#[derive(Debug, Error)]
pub enum McpTransportError {
    #[error("MCP session is closed")]
    Closed,
    #[error("MCP server does not advertise tools")]
    ToolsUnsupported,
    #[error("MCP returned an unfinished tool outcome; no follow-up call was dispatched")]
    UnfinishedTool { response: Box<CallToolResponse> },
    #[error("MCP deadline must be positive")]
    InvalidDeadline,
    #[error("MCP HTTP header {name} is managed by the protocol")]
    ReservedHeader { name: String },
    #[error("invalid MCP HTTP header name: {0}")]
    HeaderName(#[from] reqwest::header::InvalidHeaderName),
    #[error("invalid MCP HTTP header value: {0}")]
    HeaderValue(#[from] reqwest::header::InvalidHeaderValue),
    #[error("MCP {operation} exceeded its deadline; remote effects may be unknown")]
    Deadline { operation: &'static str },
    #[error("MCP {operation} failed: {source}")]
    Protocol {
        operation: &'static str,
        #[source]
        source: McpProtocolError,
    },
}

pub struct McpSession {
    client: RunningService<RoleClient, ClientConfig>,
    deadline: Duration,
    closed: bool,
}

impl McpSession {
    pub async fn connect_http(config: McpHttpConfig) -> Result<Self, McpTransportError> {
        Self::connect_http_with_protocol(config, McpHttpProtocol::Initialize2025).await
    }

    pub async fn connect_http_with_protocol(
        config: McpHttpConfig,
        protocol: McpHttpProtocol,
    ) -> Result<Self, McpTransportError> {
        if config.deadline.is_zero() {
            return Err(McpTransportError::InvalidDeadline);
        }
        let mut headers = HashMap::new();
        for (name, value) in config.headers {
            if [
                "mcp-protocol-version",
                "mcp-session-id",
                "accept",
                "content-type",
                "mcp-method",
                "last-event-id",
            ]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
            {
                return Err(McpTransportError::ReservedHeader { name });
            }
            headers.insert(name.parse()?, value.parse()?);
        }
        let mut transport_config =
            StreamableHttpClientTransportConfig::with_uri(config.url.as_str())
                .custom_headers(headers)
                .max_concurrent_requests(1)
                .control_request_timeout(config.deadline);
        // A 404 does not license replaying a mutation, even after initialization.
        transport_config.reinit_on_expired_session = false;
        transport_config.retry_config =
            Arc::new(rmcp::transport::common::client_side_sse::NeverRetry::default());
        let transport = StreamableHttpClientTransport::from_config(transport_config);
        let (version, lifecycle) = match protocol {
            McpHttpProtocol::Initialize2025 => (
                ProtocolVersion::V_2025_11_25,
                ClientLifecycleMode::Initialize,
            ),
            McpHttpProtocol::Discover2026 => (
                ProtocolVersion::V_2026_07_28,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            ),
        };
        let operation = match protocol {
            McpHttpProtocol::Initialize2025 => "initialize",
            McpHttpProtocol::Discover2026 => "server/discover",
        };
        let client = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("plasm-runtime", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(version);
        let client = tokio::time::timeout(
            config.deadline,
            Box::pin(client.serve_with_lifecycle(transport, lifecycle)),
        )
        .await
        .map_err(|_| McpTransportError::Deadline { operation })?
        .map_err(|source| McpTransportError::Protocol {
            operation,
            source: source.into(),
        })?;
        let mut session = Self {
            client,
            deadline: config.deadline,
            closed: false,
        };
        if session
            .client
            .peer_info()
            .is_none_or(|info| info.capabilities.tools.is_none())
        {
            let _ = session.shutdown().await;
            return Err(McpTransportError::ToolsUnsupported);
        }
        Ok(session)
    }

    pub async fn list_tools(
        &mut self,
        params: Option<PaginatedRequestParams>,
    ) -> Result<ListToolsResult, McpTransportError> {
        let peer = self.client.peer().clone();
        self.bounded("tools/list", Box::pin(peer.list_tools(params)))
            .await
    }

    pub async fn call_tool(
        &mut self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, McpTransportError> {
        let peer = self.client.peer().clone();
        match self
            .bounded("tools/call", Box::pin(peer.call_tool_once(params)))
            .await?
        {
            CallToolResponse::Complete(result) => Ok(result),
            response => Err(McpTransportError::UnfinishedTool {
                response: Box::new(response),
            }),
        }
    }

    async fn bounded<T>(
        &mut self,
        operation: &'static str,
        future: impl Future<Output = Result<T, ServiceError>>,
    ) -> Result<T, McpTransportError> {
        if self.closed {
            return Err(McpTransportError::Closed);
        }
        match tokio::time::timeout(self.deadline, future).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(source)) => {
                if !matches!(source, ServiceError::McpError(_)) {
                    let _ = self.shutdown().await;
                }
                Err(McpTransportError::Protocol {
                    operation,
                    source: source.into(),
                })
            }
            Err(_) => {
                let _ = self.shutdown().await;
                Err(McpTransportError::Deadline { operation })
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), McpTransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        match self.client.close_with_timeout(self.deadline).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(McpTransportError::Deadline {
                operation: "shutdown",
            }),
            Err(source) => Err(McpTransportError::Protocol {
                operation: "shutdown",
                source: source.into(),
            }),
        }
    }
}
