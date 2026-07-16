//! Unified MCP host capability policy (artifact access + MCP Apps UI).
//!
//! Single observe/match path for client semantics and cache eligibility.

use crate::mcp_client_info::McpClientInfo;
use crate::mcp_ui_capability;

use super::artifact_access::{self, ArtifactAccessMode};

/// Resolved per-transport host policy for artifact retrieval and MCP Apps UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpHostPolicy {
    pub artifact_access: ArtifactAccessMode,
    pub mcp_ui_apps: bool,
    /// Cache artifact mode when initialize was observed, or env forced a mode.
    pub cache_artifact_access: bool,
    /// Cache UI flag when initialize was observed, or `PLASM_MCP_UI_APPS` is set.
    pub cache_mcp_ui_apps: bool,
}

/// Resolve artifact + UI host policy from observed client info and optional UA hint.
///
/// `user_agent` should already be the hint from [`artifact_access::client_user_agent_hint`].
#[must_use]
pub fn resolve_mcp_host_policy(client: &McpClientInfo, user_agent: Option<&str>) -> McpHostPolicy {
    let artifact_access = artifact_access::detect_artifact_access_mode(client, user_agent);
    let mcp_ui_apps = mcp_ui_capability::client_supports_mcp_ui_apps(client);
    let env_artifact = artifact_access::artifact_access_mode_from_env().is_some();
    let env_ui = std::env::var_os("PLASM_MCP_UI_APPS").is_some();
    let initialized = client.is_initialized();
    McpHostPolicy {
        artifact_access,
        mcp_ui_apps,
        cache_artifact_access: initialized || env_artifact,
        cache_mcp_ui_apps: initialized || env_ui,
    }
}

pub(crate) fn log_resolved_host_policy(client: &McpClientInfo, policy: &McpHostPolicy) {
    match client {
        McpClientInfo::Initialized(params) => {
            tracing::info!(
                client_info.name = %params.client_info.name,
                client_info.version = %params.client_info.version,
                artifact_access_mode = ?policy.artifact_access,
                mcp_ui_apps_supported = policy.mcp_ui_apps,
                cache_artifact_access = policy.cache_artifact_access,
                cache_mcp_ui_apps = policy.cache_mcp_ui_apps,
                "resolved MCP host policy"
            );
        }
        McpClientInfo::Default => {
            tracing::info!(
                artifact_access_mode = ?policy.artifact_access,
                mcp_ui_apps_supported = policy.mcp_ui_apps,
                cache_artifact_access = policy.cache_artifact_access,
                cache_mcp_ui_apps = policy.cache_mcp_ui_apps,
                "resolved MCP host policy (McpClientInfo::Default)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{ClientCapabilities, Implementation, InitializeRequestParams};

    fn initialized(name: &str) -> McpClientInfo {
        McpClientInfo::from_initialize(InitializeRequestParams {
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: name.into(),
                version: "1.0.0".into(),
                title: None,
                description: None,
                icons: vec![],
                website_url: None,
            },
            protocol_version: "2025-11-25".into(),
            meta: None,
        })
    }

    #[test]
    fn default_does_not_cache_without_env() {
        let policy = resolve_mcp_host_policy(&McpClientInfo::Default, None);
        assert!(!policy.cache_artifact_access);
        assert!(!policy.cache_mcp_ui_apps);
        assert!(!policy.mcp_ui_apps);
        assert_eq!(policy.artifact_access, ArtifactAccessMode::ResourcesRead);
    }

    #[test]
    fn initialized_caches_both() {
        let policy = resolve_mcp_host_policy(&initialized("cursor"), None);
        assert!(policy.cache_artifact_access);
        assert!(policy.cache_mcp_ui_apps);
    }
}
