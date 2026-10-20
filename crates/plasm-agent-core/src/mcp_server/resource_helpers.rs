//! MCP resource presentation and transport error helpers.

use super::*;

pub(crate) fn mcp_key(runtime: &Arc<McpRequest>) -> Result<String, ToolFailure> {
    Some(runtime.transport_key.clone()).ok_or_else(|| {
        ToolFailure::rejected(
            "MCP session not ready: complete the MCP initialize handshake before calling tools.",
        )
    })
}

pub(crate) fn mcp_call_tool_error_class(err: &ToolFailure) -> &'static str {
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

/// Stored results use the live rowset renderer and its display limits in model context.
pub(crate) async fn run_artifact_context_payload(
    handler: &PlasmMcpHandler,
    prompt_hash: &str,
    session_id: &str,
    payload: &ArtifactPayload,
) -> Result<ArtifactPayload, ToolFailure> {
    let doc: crate::run_artifacts::RunArtifactDocument = serde_json::from_slice(&payload.bytes)
        .map_err(|error| ToolFailure::rejected(format!("run artifact decode failed: {error}")))?;
    let session = handler
        .plasm
        .get_execute_session(prompt_hash, session_id)
        .await
        .ok_or_else(|| {
            ToolFailure::rejected(
                "execute session unavailable: extend plasm_context with the same logical_session_ref and current intent",
            )
        })?;
    let logical_session_ref = handler
        .plasm
        .logical_session_id_for_execute_binding(prompt_hash, session_id)
        .await
        .map(crate::mcp_logical_ref::format_logical_session_wire_ref_from_uuid)
        .ok_or_else(|| ToolFailure::rejected("run artifact has no MCP logical session binding: use the run URI returned by plasm_run for the current logical_session_ref"))?;
    let (out, _) = crate::terminal_result_format::hydrate_plan_run_from_artifact_formatted(
        &doc,
        session.as_ref(),
        &doc.run_id,
        Some(&logical_session_ref),
    )
    .map_err(|error| ToolFailure::rejected(error.to_string()))?;
    Ok(ArtifactPayload {
        metadata: crate::run_artifacts::ArtifactPayloadMetadata {
            content_type: "text/markdown".into(),
            ..crate::run_artifacts::ArtifactPayloadMetadata::json_default()
        },
        bytes: out.markdown.into(),
    })
}

pub(crate) fn read_resource_result_for_payload(
    uri: &str,
    payload: ArtifactPayload,
) -> Result<ReadResourceResult, ErrorData> {
    let content = match std::str::from_utf8(&payload.bytes) {
        Ok(text) => ResourceContents::text(text, uri),
        Err(_) => ResourceContents::blob(
            base64::engine::general_purpose::STANDARD.encode(&payload.bytes),
            uri,
        ),
    }
    .with_mime_type(payload.metadata.content_type);
    Ok(ReadResourceResult::new(vec![content]))
}
