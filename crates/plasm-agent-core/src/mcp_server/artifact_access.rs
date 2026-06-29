//! Per-transport artifact retrieval mode detection (see [`crate::mcp_run_markdown::ArtifactAccessMode`]).

pub(crate) use crate::mcp_run_markdown::ArtifactAccessMode;

use rust_mcp_sdk::schema::InitializeRequestParams;

/// Verified `initialize.clientInfo.name` values where the agent toolkit cannot reliably invoke
/// MCP `resources/read` (see docs/mcp-client-conformance.md).
pub(crate) const TOOL_ONLY_WIRE_NAMES: &[&str] = &[
    "claude-code",
    "claude-api-mcp-connector",
    "anthropic/api",
    "anthropic/claudeai",
];

/// Optional server override via `PLASM_MCP_ARTIFACT_ACCESS=tool|resources`.
pub(crate) fn artifact_access_mode_from_env() -> Option<ArtifactAccessMode> {
    match std::env::var("PLASM_MCP_ARTIFACT_ACCESS")
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "tool" | "tool_fallback" | "tool-fallback" => Some(ArtifactAccessMode::ToolFallback),
        "resources" | "resources_read" | "resources-read" => {
            Some(ArtifactAccessMode::ResourcesRead)
        }
        _ => None,
    }
}

/// HTTP User-Agent hint: captured MCP transport UA, then `PLASM_MCP_CLIENT_USER_AGENT`.
pub(crate) fn client_user_agent_hint(http_ua: Option<&str>) -> Option<String> {
    http_ua
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("PLASM_MCP_CLIENT_USER_AGENT")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

/// Detect artifact access mode from initialize metadata and optional HTTP user-agent.
pub(crate) fn detect_artifact_access_mode(
    client: Option<&InitializeRequestParams>,
    user_agent: Option<&str>,
) -> ArtifactAccessMode {
    if let Some(mode) = artifact_access_mode_from_env() {
        return mode;
    }
    if let Some(params) = client {
        let name = params.client_info.name.to_ascii_lowercase();
        let version = params.client_info.version.to_ascii_lowercase();
        if is_known_tool_only_mcp_host(&name, &version) {
            return ArtifactAccessMode::ToolFallback;
        }
    }
    if let Some(ua) = user_agent {
        let ua_lower = ua.to_ascii_lowercase();
        if ua_lower.contains("anthropic") && ua_lower.contains("mcp") {
            return ArtifactAccessMode::ToolFallback;
        }
    }
    ArtifactAccessMode::ResourcesRead
}

fn is_known_tool_only_mcp_host(name: &str, version: &str) -> bool {
    if TOOL_ONLY_WIRE_NAMES.contains(&name) {
        return true;
    }
    let combined = format!("{name} {version}");
    combined.contains("connector") && (name.contains("claude") || name.contains("anthropic"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{ClientCapabilities, Implementation, LATEST_PROTOCOL_VERSION};

    const RESOURCES_READ_WIRE_NAMES: &[&str] = &["cursor-vscode", "claude-ai", "cline"];

    fn client(name: &str, version: &str) -> InitializeRequestParams {
        InitializeRequestParams {
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: name.into(),
                version: version.into(),
                title: None,
                description: None,
                icons: vec![],
                website_url: None,
            },
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            meta: None,
        }
    }

    #[test]
    fn default_is_resources_read() {
        assert_eq!(
            detect_artifact_access_mode(None, None),
            ArtifactAccessMode::ResourcesRead
        );
    }

    #[test]
    fn tool_only_wire_names_are_tool_fallback() {
        for wire in TOOL_ONLY_WIRE_NAMES {
            let display = match *wire {
                "anthropic/api" => "Anthropic/API",
                "anthropic/claudeai" => "Anthropic/ClaudeAI",
                other => other,
            };
            let params = client(display, "1.0.0");
            assert_eq!(
                detect_artifact_access_mode(Some(&params), None),
                ArtifactAccessMode::ToolFallback,
                "wire name {wire}"
            );
        }
    }

    #[test]
    fn resources_read_wire_names_stay_resources_read() {
        for wire in RESOURCES_READ_WIRE_NAMES {
            let display = match *wire {
                "cline" => "Cline",
                other => other,
            };
            let params = client(display, "1.0.0");
            assert_eq!(
                detect_artifact_access_mode(Some(&params), None),
                ArtifactAccessMode::ResourcesRead,
                "wire name {wire}"
            );
        }
    }

    #[test]
    fn undocumented_anthropic_connector_substring_is_tool_fallback() {
        let params = client("my-claude-connector-beta", "0.1");
        assert_eq!(
            detect_artifact_access_mode(Some(&params), None),
            ArtifactAccessMode::ToolFallback
        );
    }

    #[test]
    fn anthropic_mcp_user_agent_is_tool_fallback() {
        assert_eq!(
            detect_artifact_access_mode(None, Some("anthropic-mcp-connector/1.0"),),
            ArtifactAccessMode::ToolFallback
        );
    }
}
