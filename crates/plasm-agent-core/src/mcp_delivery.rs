//! MCP dual-lane delivery profile: which `CallToolResult` lanes a host may receive.
//!
//! Resolved once per tool call from Apps capability + [`ArtifactAccessMode`]. Illegal
//! combinations such as `structuredContent` without Apps are unrepresentable.

use crate::mcp_run_markdown::ArtifactAccessMode;

/// Which lanes a finalized [`CallToolResult`](rust_mcp_sdk::schema::CallToolResult) exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpDeliveryProfile {
    /// MCP Apps disabled: content only; no `_meta.ui`, no `structuredContent`.
    ContentOnly,
    /// Tool-only host (Claude ToolFallback): content + optional `_meta.ui` mount; no `structuredContent`.
    ToolFallback,
    /// Full MCP App host: content + `_meta.ui` + `structuredContent.ui`.
    FullApps,
}

impl McpDeliveryProfile {
    /// Resolve delivery once from host capabilities.
    pub fn resolve(ui_apps_enabled: bool, artifact_access: ArtifactAccessMode) -> Self {
        match (ui_apps_enabled, artifact_access) {
            (false, _) => Self::ContentOnly,
            (true, ArtifactAccessMode::ToolFallback) => Self::ToolFallback,
            (true, ArtifactAccessMode::ResourcesRead) => Self::FullApps,
        }
    }

    /// Whether `_meta.ui.resourceUri` (iframe mount) should be attached.
    pub fn attaches_ui_meta(self) -> bool {
        !matches!(self, Self::ContentOnly)
    }

    /// Whether `structuredContent.ui` should be emitted.
    pub fn emits_structured_ui(self) -> bool {
        matches!(self, Self::FullApps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_profile_resolve_truth_table() {
        assert_eq!(
            McpDeliveryProfile::resolve(false, ArtifactAccessMode::ResourcesRead),
            McpDeliveryProfile::ContentOnly
        );
        assert_eq!(
            McpDeliveryProfile::resolve(false, ArtifactAccessMode::ToolFallback),
            McpDeliveryProfile::ContentOnly
        );
        assert_eq!(
            McpDeliveryProfile::resolve(true, ArtifactAccessMode::ToolFallback),
            McpDeliveryProfile::ToolFallback
        );
        assert_eq!(
            McpDeliveryProfile::resolve(true, ArtifactAccessMode::ResourcesRead),
            McpDeliveryProfile::FullApps
        );
        assert!(McpDeliveryProfile::FullApps.emits_structured_ui());
        assert!(McpDeliveryProfile::FullApps.attaches_ui_meta());
        assert!(!McpDeliveryProfile::ToolFallback.emits_structured_ui());
        assert!(McpDeliveryProfile::ToolFallback.attaches_ui_meta());
        assert!(!McpDeliveryProfile::ContentOnly.attaches_ui_meta());
    }
}
