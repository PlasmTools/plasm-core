//! MCP Apps capability negotiation (`io.modelcontextprotocol/ui`, SEP-1865).
//!
//! Hosts advertise `capabilities.extensions["io.modelcontextprotocol/ui"].mimeTypes` on
//! `initialize`; servers echo the same extension when the client supports MCP Apps and gate
//! UI tool metadata / `structuredContent.ui` when not.

use std::collections::BTreeMap;

use rust_mcp_sdk::schema::{ClientCapabilities, InitializeRequestParams, ServerCapabilities};
use serde_json::{Map, Value};

/// Extension identifier from [MCP Apps spec](https://github.com/modelcontextprotocol/ext-apps).
pub const MCP_UI_EXTENSION_ID: &str = "io.modelcontextprotocol/ui";

/// HTML MCP App resource MIME type (MVP).
pub const MCP_UI_RESOURCE_MIME: &str = "text/html;profile=mcp-app";

/// Wire `clientInfo.name` values known to render MCP Apps even when `extensions` is omitted.
pub(crate) const KNOWN_MCP_APP_HOSTS: &[&str] = &[
    "cursor-vscode",
    "cline",
    "visual studio code",
    "visual-studio-code",
    "claude-ai",
    "claude-code",
    "claude-desktop",
];

/// Per-extension settings echoed on `initialize` when MCP Apps are negotiated.
pub fn ui_extension_settings() -> Map<String, Value> {
    serde_json::json!({
        "mimeTypes": [MCP_UI_RESOURCE_MIME]
    })
    .as_object()
    .cloned()
    .expect("ui extension settings object")
}

/// Server `capabilities.extensions` payload for negotiated MCP App hosts.
pub fn server_ui_extensions() -> BTreeMap<String, Map<String, Value>> {
    let mut ext = BTreeMap::new();
    ext.insert(MCP_UI_EXTENSION_ID.into(), ui_extension_settings());
    ext
}

/// Apply MCP Apps extension to an initialize capabilities object when enabled.
pub fn apply_server_ui_extensions(capabilities: &mut ServerCapabilities, enabled: bool) {
    if enabled {
        capabilities.extensions = Some(server_ui_extensions());
    } else {
        capabilities.extensions = None;
    }
}

fn mcp_ui_apps_mode_from_env() -> Option<bool> {
    match std::env::var("PLASM_MCP_UI_APPS")
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "always" | "force" => Some(true),
        "0" | "false" | "no" | "never" | "off" => Some(false),
        "auto" => None,
        _ => None,
    }
}

fn mime_types_include_mcp_app(mime_types: &[Value]) -> bool {
    mime_types
        .iter()
        .any(|v| v.as_str() == Some(MCP_UI_RESOURCE_MIME))
}

/// `getUiCapability` equivalent: client advertised `text/html;profile=mcp-app`.
pub fn client_capabilities_support_mcp_ui_apps(capabilities: &ClientCapabilities) -> bool {
    let Some(ext) = capabilities.extensions.as_ref() else {
        return false;
    };
    let Some(ui) = ext.get(MCP_UI_EXTENSION_ID) else {
        return false;
    };
    let Some(mime_types) = ui.get("mimeTypes").and_then(|v| v.as_array()) else {
        return false;
    };
    mime_types_include_mcp_app(mime_types)
}

fn known_mcp_app_host(client: &InitializeRequestParams) -> bool {
    let name = client.client_info.name.to_ascii_lowercase();
    KNOWN_MCP_APP_HOSTS
        .iter()
        .any(|host| name == *host || name.contains(host))
}

/// Whether this transport should register MCP App UI tools and per-result UI lanes.
pub fn client_supports_mcp_ui_apps(client: Option<&InitializeRequestParams>) -> bool {
    if let Some(forced) = mcp_ui_apps_mode_from_env() {
        return forced;
    }
    let Some(params) = client else {
        return false;
    };
    if client_capabilities_support_mcp_ui_apps(&params.capabilities) {
        return true;
    }
    known_mcp_app_host(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{Implementation, LATEST_PROTOCOL_VERSION};

    fn client_with_extensions(extensions: BTreeMap<String, Map<String, Value>>) -> InitializeRequestParams {
        InitializeRequestParams {
            capabilities: ClientCapabilities {
                extensions: Some(extensions),
                ..Default::default()
            },
            client_info: Implementation {
                name: "mcp-ui-test".into(),
                version: "0".into(),
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
    fn extension_mime_types_gate_ui_apps() {
        let mut ext = BTreeMap::new();
        ext.insert(MCP_UI_EXTENSION_ID.into(), ui_extension_settings());
        let params = client_with_extensions(ext);
        assert!(client_supports_mcp_ui_apps(Some(&params)));
    }

    #[test]
    fn missing_extension_falls_back_to_known_host() {
        let params = InitializeRequestParams {
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "cursor-vscode".into(),
                version: "1".into(),
                title: None,
                description: None,
                icons: vec![],
                website_url: None,
            },
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            meta: None,
        };
        assert!(client_supports_mcp_ui_apps(Some(&params)));
    }

    #[test]
    fn unknown_host_without_extension_is_disabled() {
        let params = InitializeRequestParams {
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "unknown-mcp-client".into(),
                version: "0".into(),
                title: None,
                description: None,
                icons: vec![],
                website_url: None,
            },
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            meta: None,
        };
        assert!(!client_supports_mcp_ui_apps(Some(&params)));
    }

    #[test]
    fn server_extensions_serialize_mime_types() {
        let ext = server_ui_extensions();
        let mime = ext
            .get(MCP_UI_EXTENSION_ID)
            .and_then(|m| m.get("mimeTypes"))
            .and_then(|v| v.as_array())
            .expect("mimeTypes");
        assert!(mime_types_include_mcp_app(mime));
    }
}
