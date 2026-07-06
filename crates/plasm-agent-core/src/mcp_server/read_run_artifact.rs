//! MCP run snapshot resolution shared by `plasm_read_run_artifact` and `plasm_ui_read_run`.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolResult, TextContent};
use rust_mcp_sdk::McpServer;
use serde_json::json;
use uuid::Uuid;

use crate::incoming_auth::tenant_scope;
use crate::run_artifacts::{strip_plasm_resource_read_source, ArtifactPayload, RunArtifactId};
use crate::session_identity::LogicalSessionId;

use super::artifact_resolve::{
    resolve_lookup_arg, resolve_run_artifact_for_binding, RunArtifactLookupArg,
    RunArtifactResolveError,
};
use super::discover::mcp_artifact_payload_chars;
use super::tool_parse::parse_logical_session_ref_arg;
use super::PlasmMcpHandler;

pub(crate) struct ResolvedRunSnapshot {
    pub uri_for_trace: String,
    pub run_id: Option<RunArtifactId>,
    pub prompt_hash: String,
    pub session_id: String,
    pub payload: ArtifactPayload,
}

pub(crate) fn parse_read_run_artifact_lookup(
    tool_name: &str,
    v: &serde_json::Value,
) -> Result<(String, RunArtifactLookupArg), CallToolError> {
    let Some(obj) = v.as_object() else {
        return Err(CallToolError::invalid_arguments(
            tool_name,
            Some("arguments must be a JSON object".into()),
        ));
    };
    for key in ["plan_commit_ref", "page_handle", "program", "run_ref"] {
        if obj.contains_key(key) {
            return Err(CallToolError::invalid_arguments(
                tool_name,
                Some(format!("`{key}` is not accepted on {tool_name}")),
            ));
        }
    }
    let session_ref = parse_logical_session_ref_arg(tool_name, v)?;
    if obj.contains_key("resource_index") {
        return Err(CallToolError::invalid_arguments(
            tool_name,
            Some(
                "`resource_index` is not accepted (ambiguous with plan `run_step`) — use `run_id` or `artifact_uri` from `_meta.plasm.steps[]`"
                    .into(),
            ),
        ));
    }
    let has_uri = obj.get("artifact_uri").is_some();
    let has_run_id = obj.get("run_id").is_some();
    let count = [has_uri, has_run_id].into_iter().filter(|b| *b).count();
    if count != 1 {
        return Err(CallToolError::invalid_arguments(
            tool_name,
            Some(
                "provide exactly one of `artifact_uri` or `run_id` from the same `plasm_run` step"
                    .into(),
            ),
        ));
    }
    let lookup = if has_uri {
        let uri = obj
            .get("artifact_uri")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool_name,
                    Some("`artifact_uri` must be a non-empty string".into()),
                )
            })?;
        RunArtifactLookupArg::ArtifactUri(uri.to_string())
    } else {
        let wire = obj
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool_name,
                    Some("`run_id` must be a non-empty string".into()),
                )
            })?;
        let run_id = RunArtifactId::from_wire(wire).ok_or_else(|| {
            CallToolError::invalid_arguments(tool_name, Some(format!("invalid run_id: {wire}")))
        })?;
        RunArtifactLookupArg::RunId(run_id)
    };
    Ok((session_ref, lookup))
}

pub(crate) fn map_resolve_err(tool_name: &str, e: RunArtifactResolveError) -> CallToolError {
    match e {
        RunArtifactResolveError::UnknownRunId | RunArtifactResolveError::UnknownPlan => {
            CallToolError::invalid_arguments(tool_name, Some(e.to_string()))
        }
        RunArtifactResolveError::LegacyResourceIndexUri(_)
        | RunArtifactResolveError::UnsupportedUri(_)
        | RunArtifactResolveError::SessionMismatch
        | RunArtifactResolveError::InvalidSessionRef => {
            CallToolError::invalid_arguments(tool_name, Some(e.to_string()))
        }
        RunArtifactResolveError::DecodeFailed(msg) => {
            CallToolError::from_message(format!("run artifact decode failed: {msg}"))
        }
        RunArtifactResolveError::Integrity(msg) => {
            CallToolError::from_message(format!("run artifact integrity check failed: {msg}"))
        }
    }
}

impl PlasmMcpHandler {
    pub(crate) async fn resolve_run_snapshot_for_tool(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
        tool_name: &str,
        logical_uuid: Uuid,
        session_ref: &str,
        lookup_arg: RunArtifactLookupArg,
    ) -> Result<ResolvedRunSnapshot, CallToolError> {
        let principal_incoming = self.ensure_mcp_principal(transport_key, runtime).await?;
        let scope = tenant_scope(principal_incoming.as_ref());
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(LogicalSessionId(logical_uuid), &scope)
            .await
        {
            return Err(CallToolError::from_message(
                "logical_session_ref is unknown or does not belong to this tenant scope",
            ));
        }
        let ls_key = logical_uuid.to_string();
        let binding = self
            .resolve_binding_for_logical(transport_key, logical_uuid)
            .await
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool_name,
                    Some(
                        "no execute session for this logical session: call plasm_context with capability picks (`seeds`) first"
                            .into(),
                    ),
                )
            })?;
        let started = Instant::now();
        let (uri_for_trace, run_lookup) = match lookup_arg {
            RunArtifactLookupArg::ArtifactUri(raw) => {
                let (uri_owned, _) = strip_plasm_resource_read_source(raw.as_str());
                resolve_lookup_arg(
                    session_ref,
                    &binding,
                    ls_key.as_str(),
                    RunArtifactLookupArg::ArtifactUri(uri_owned),
                )
                .map_err(|e| map_resolve_err(tool_name, e))?
            }
            other => resolve_lookup_arg(session_ref, &binding, ls_key.as_str(), other)
                .map_err(|e| map_resolve_err(tool_name, e))?,
        };
        let resolved = resolve_run_artifact_for_binding(
            self.plasm.as_ref(),
            &binding,
            run_lookup,
            Some(ls_key.as_str()),
            Some("tool"),
            started,
            uri_for_trace.as_str(),
        )
        .await
        .map_err(|e| map_resolve_err(tool_name, e))?;
        crate::metrics::record_mcp_resource_read("canonical", "success", "tool", started.elapsed());
        Ok(ResolvedRunSnapshot {
            uri_for_trace,
            run_id: resolved.run_id,
            prompt_hash: resolved.prompt_hash,
            session_id: resolved.session_id,
            payload: resolved.payload,
        })
    }

    pub(crate) async fn handle_read_run_artifact(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, CallToolError> {
        const TOOL: &str = "plasm_read_run_artifact";
        let mode = self
            .resolved_artifact_access_mode(transport_key, runtime)
            .await;
        if !mode.exposes_read_tool() {
            return Err(CallToolError::from_message(
                "plasm_read_run_artifact is not available on this MCP transport (use MCP resources/read)",
            ));
        }
        let (session_ref, lookup_arg) = parse_read_run_artifact_lookup(TOOL, v)?;
        let logical_uuid = self.resolve_logical_session_ref_to_uuid(TOOL, &session_ref)?;
        let resolved = self
            .resolve_run_snapshot_for_tool(
                transport_key,
                runtime,
                TOOL,
                logical_uuid,
                session_ref.as_str(),
                lookup_arg,
            )
            .await?;
        let payload =
            crate::run_artifacts::project_artifact_payload_for_agent(&resolved.payload, false)
                .map_err(|e| {
                    CallToolError::from_message(format!("artifact projection failed: {e}"))
                })?;
        let (char_count, binary) = mcp_artifact_payload_chars(&payload);
        let text = std::str::from_utf8(&payload.bytes)
            .map(str::to_string)
            .unwrap_or_else(|_| {
                base64::engine::general_purpose::STANDARD.encode(&resolved.payload.bytes)
            });
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            json!({
                "artifact_uri": resolved.uri_for_trace,
                "run_id": resolved.run_id.as_ref().map(|r| r.to_wire()),
                "byte_count": payload.bytes.len(),
                "char_count": char_count,
                "binary": binary,
                "prompt_hash": resolved.prompt_hash,
                "session_id": resolved.session_id,
            }),
        );
        Ok(
            CallToolResult::text_content(vec![TextContent::new(text, None, None)])
                .with_meta(Some(meta)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_exactly_one_lookup_key() {
        let err = parse_read_run_artifact_lookup(
            "plasm_read_run_artifact",
            &serde_json::json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            }),
        )
        .expect_err("missing lookup");
        assert!(err.to_string().contains("exactly one"));
    }

    #[test]
    fn parse_rejects_resource_index_lookup() {
        let err = parse_read_run_artifact_lookup(
            "plasm_read_run_artifact",
            &serde_json::json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
                "resource_index": 3,
            }),
        )
        .expect_err("resource_index");
        assert!(err.to_string().contains("resource_index"));
    }

    #[test]
    fn parse_rejects_transitional_keys() {
        let err = parse_read_run_artifact_lookup(
            "plasm_ui_read_run",
            &serde_json::json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
                "run_ref": "pc1",
                "run_id": "pr".to_string() + &"a".repeat(64),
            }),
        )
        .expect_err("run_ref");
        assert!(err.to_string().contains("run_ref"));
    }
}
