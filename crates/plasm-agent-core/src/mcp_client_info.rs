//! MCP client identity for host capability policy.
//!
//! Initialize is modeled as a closed enum — never `Option` — so default behavior is
//! explicit when handshake has not been observed. Callers MUST NOT await initialization
//! forever; [`McpClientInfo::Default`] governs until `initialize` arrives.

use rust_mcp_sdk::schema::InitializeRequestParams;
use rust_mcp_sdk::McpServer;

/// Observed MCP client initialize state for artifact / UI host policy.
#[derive(Debug, Clone, Default)]
pub enum McpClientInfo {
    /// No `initialize` params observed yet.
    ///
    /// Default host policy:
    /// - artifact access: [`crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead`]
    ///   (env override still wins when set)
    /// - MCP Apps UI: disabled (env force still wins when set)
    #[default]
    Default,
    /// Client `initialize` request params have been observed.
    Initialized(Box<InitializeRequestParams>),
}

impl McpClientInfo {
    /// Snapshot client info from the runtime without waiting for handshake.
    #[must_use]
    pub fn observe(runtime: &dyn McpServer) -> Self {
        match runtime.client_info() {
            Some(params) => Self::Initialized(Box::new(params)),
            None => Self::Default,
        }
    }

    /// Construct from an `initialize` handler payload (always observed).
    #[must_use]
    pub fn from_initialize(params: InitializeRequestParams) -> Self {
        Self::Initialized(Box::new(params))
    }

    /// Whether initialize params have been observed (safe to permanently cache policy).
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        matches!(self, Self::Initialized(_))
    }

    /// Borrow initialize params when observed.
    #[must_use]
    pub fn as_initialized(&self) -> Option<&InitializeRequestParams> {
        match self {
            Self::Initialized(params) => Some(params.as_ref()),
            Self::Default => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_explicit_variant() {
        let info = McpClientInfo::default();
        assert!(matches!(info, McpClientInfo::Default));
        assert!(!info.is_initialized());
        assert!(info.as_initialized().is_none());
    }
}
