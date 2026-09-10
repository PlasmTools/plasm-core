//! MCP resource presentation and transport error helpers.

use super::*;

pub(crate) fn mcp_key(runtime: &Arc<dyn McpServer>) -> Result<String, CallToolError> {
    runtime.session_id().ok_or_else(|| {
        CallToolError::from_message(
            "MCP session not ready: complete the MCP initialize handshake before calling tools.",
        )
    })
}

pub(crate) fn mcp_call_tool_error_class(err: &CallToolError) -> &'static str {
    let msg = err.to_string();
    if msg.contains("entry_id not allowed by tenant MCP configuration") {
        return "entry_not_allowed";
    }
    if msg.contains("incoming auth required") {
        return "incoming_auth_required";
    }
    if msg.contains("MCP Authorization missing tenant binding") {
        return "missing_tenant_binding";
    }
    if msg.contains("Tenant MCP configuration is no longer available") {
        return "tenant_mcp_unavailable";
    }
    if msg.contains("Personal MCP configuration is missing owner binding") {
        return "owner_binding_missing";
    }
    if msg.contains("MCP Authorization required") {
        return "mcp_authorization_required";
    }
    "call_tool_error"
}

pub(crate) fn mcp_truncate_resource_uri_display(uri: &str) -> String {
    const MAX: usize = 160;
    if uri.chars().count() <= MAX {
        uri.to_string()
    } else {
        format!(
            "{}…",
            uri.chars().take(MAX.saturating_sub(1)).collect::<String>()
        )
    }
}

pub(crate) fn mcp_artifact_payload_chars(payload: &ArtifactPayload) -> (u64, bool) {
    match std::str::from_utf8(&payload.bytes) {
        Ok(s) => (s.chars().count() as u64, false),
        Err(_) => (payload.bytes.len() as u64, true),
    }
}

pub(crate) fn read_resource_result_for_payload(
    uri: &str,
    payload: ArtifactPayload,
) -> Result<ReadResourceResult, RpcError> {
    let maybe_utf8 = std::str::from_utf8(&payload.bytes)
        .ok()
        .map(|s| s.to_string());
    Ok(ReadResourceResult {
        contents: vec![if let Some(text) = maybe_utf8 {
            ReadResourceContent::TextResourceContents(TextResourceContents {
                meta: None,
                mime_type: Some(payload.metadata.content_type),
                text,
                uri: uri.to_string(),
            })
        } else {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&payload.bytes);
            ReadResourceContent::BlobResourceContents(
                BlobResourceContents::new(b64, uri.to_string())
                    .with_mime_type(payload.metadata.content_type),
            )
        }],
        meta: None,
    })
}
