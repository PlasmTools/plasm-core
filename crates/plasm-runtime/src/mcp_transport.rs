//! Outbound MCP sessions. Configuration belongs to the host, never to a catalog.
//!
//! Tool results retain their protocol envelope. In particular, `isError` is a
//! received tool outcome, not a connection failure. Calls are never retried.

use async_trait::async_trait;
use rust_mcp_sdk::{
    error::McpSdkError,
    mcp_client::{client_runtime, ClientHandler},
    schema::{
        ClientCapabilities, Implementation, InitializeRequestParams, LATEST_PROTOCOL_VERSION,
    },
    McpClient, RequestOptions, StreamableTransportOptions,
};
use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};
use thiserror::Error;

pub use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
};

/// Host-owned endpoint and headers; catalogs cannot provide credentials or URLs.
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
    #[error("task-augmented MCP calls are not supported by this transport")]
    TaskAugmentationUnsupported,
    #[error("MCP deadline must be positive")]
    InvalidDeadline,
    #[error("MCP HTTP header {name} is managed by the protocol")]
    ReservedHeader { name: String },
    #[error("MCP {operation} exceeded its deadline; remote effects may be unknown")]
    Deadline { operation: &'static str },
    #[error("MCP {operation} failed: {source}")]
    Protocol {
        operation: &'static str,
        #[source]
        source: McpSdkError,
    },
}

struct Handler;
#[async_trait]
impl ClientHandler for Handler {}

/// An initialized connection. Mutable access serializes calls with shutdown.
/// A timed-out connection is retired, so it cannot silently replay uncertain work.
pub struct McpSession {
    client: Arc<dyn McpClient>,
    deadline: Duration,
    closed: bool,
}

impl McpSession {
    pub async fn connect_http(mut config: McpHttpConfig) -> Result<Self, McpTransportError> {
        if config.deadline.is_zero() {
            return Err(McpTransportError::InvalidDeadline);
        }
        for name in config.headers.keys() {
            if [
                "mcp-protocol-version",
                "mcp-session-id",
                "accept",
                "content-type",
            ]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
            {
                return Err(McpTransportError::ReservedHeader { name: name.clone() });
            }
        }
        config.headers.insert(
            "MCP-Protocol-Version".into(),
            LATEST_PROTOCOL_VERSION.into(),
        );
        let client = client_runtime::with_transport_options(
            client_details(),
            StreamableTransportOptions {
                mcp_url: config.url.to_string(),
                request_options: RequestOptions {
                    request_timeout: config.deadline,
                    max_retries: Some(0),
                    custom_headers: Some(config.headers),
                    ..RequestOptions::default()
                },
            },
            Handler,
            None,
            None,
            None,
        );
        Self::initialize(client, config.deadline).await
    }

    async fn initialize(
        client: Arc<dyn McpClient>,
        deadline: Duration,
    ) -> Result<Self, McpTransportError> {
        let mut session = Self {
            client,
            deadline,
            closed: false,
        };
        let client = session.client.clone();
        if let Err(error) = session.bounded("initialize", client.start()).await {
            let _ = session.shutdown().await;
            return Err(error);
        }
        if session.client.server_has_tools() != Some(true) {
            let _ = session.shutdown().await;
            return Err(McpTransportError::ToolsUnsupported);
        }
        Ok(session)
    }

    /// Returns one page, retaining `nextCursor`; discovery policy is caller-owned.
    pub async fn list_tools(
        &mut self,
        params: Option<PaginatedRequestParams>,
    ) -> Result<ListToolsResult, McpTransportError> {
        let client = self.client.clone();
        self.bounded("tools/list", client.request_tool_list(params))
            .await
    }

    /// A successful exchange may contain `isError: true`; no content is discarded.
    pub async fn call_tool(
        &mut self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, McpTransportError> {
        if params.task.is_some() {
            return Err(McpTransportError::TaskAugmentationUnsupported);
        }
        let client = self.client.clone();
        self.bounded("tools/call", client.request_tool_call(params))
            .await
    }

    async fn bounded<T>(
        &mut self,
        operation: &'static str,
        future: impl Future<Output = Result<T, McpSdkError>>,
    ) -> Result<T, McpTransportError> {
        if self.closed {
            return Err(McpTransportError::Closed);
        }
        match tokio::time::timeout(self.deadline, future).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(source)) => {
                if !matches!(source, McpSdkError::RpcError(_)) {
                    let _ = self.shutdown().await;
                }
                Err(McpTransportError::Protocol { operation, source })
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
        match tokio::time::timeout(self.deadline, self.client.shut_down()).await {
            Ok(result) => result.map_err(|source| McpTransportError::Protocol {
                operation: "shutdown",
                source,
            }),
            Err(_) => Err(McpTransportError::Deadline {
                operation: "shutdown",
            }),
        }
    }
}

fn client_details() -> InitializeRequestParams {
    InitializeRequestParams {
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "plasm-runtime".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: None,
            description: None,
            icons: vec![],
            website_url: None,
        },
        protocol_version: LATEST_PROTOCOL_VERSION.into(),
        meta: None,
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if !self.closed {
            let client = self.client.clone();
            let deadline = self.deadline;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(deadline, client.shut_down()).await;
                });
            }
        }
    }
}
