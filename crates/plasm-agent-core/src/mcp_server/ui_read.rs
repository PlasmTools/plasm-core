//! MCP App-only hydration tools (`_meta.ui.visibility: ["app"]` — host-enforced, not server-filtered).

use std::sync::Arc;

use plasm_core::PlanCommitRef;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::CallToolResult;
use rust_mcp_sdk::McpServer;
use serde_json::Value;

use crate::incoming_auth::tenant_scope;
use crate::plan_commit_store::{dry_for_committed_plasm_run, resolve_committed_plan};
use crate::plan_ux_reflection::{plan_ux_reflection_value, PlanUxBuildContext};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::session_identity::LogicalSessionId;

use super::read_run_artifact::parse_read_run_artifact_lookup;
use super::tool_parse::parse_logical_session_ref_arg;
use super::PlasmMcpHandler;

impl PlasmMcpHandler {
    pub(crate) async fn handle_ui_read_plan(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &Value,
    ) -> Result<CallToolResult, CallToolError> {
        const TOOL: &str = "plasm_ui_read_plan";
        let principal_incoming = self.ensure_mcp_principal(transport_key, runtime).await?;
        let session_ref = parse_logical_session_ref_arg(TOOL, v)?;
        let run_ref_str = v
            .get("run_ref")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    TOOL,
                    Some("missing or invalid `run_ref`: non-empty string (pcN from `plasm`)".into()),
                )
            })?;
        let commit_ref = PlanCommitRef::parse(run_ref_str).ok_or_else(|| {
            CallToolError::invalid_arguments(
                TOOL,
                Some(format!("invalid `run_ref`: expected pcN token, got {run_ref_str:?}")),
            )
        })?;
        let logical_uuid =
            self.resolve_logical_session_ref_to_uuid(TOOL, &session_ref)?;
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
        let binding = self
            .resolve_binding_for_logical(transport_key, logical_uuid)
            .await
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    TOOL,
                    Some(
                        "no execute session for this logical session: call plasm_context first"
                            .into(),
                    ),
                )
            })?;
        let es = self
            .plasm
            .get_execute_session(binding.prompt_hash.as_str(), binding.session_id.as_str())
            .await
            .ok_or_else(|| {
                CallToolError::from_message("execute session expired — reopen plasm_context")
            })?;
        let committed = resolve_committed_plan(es.as_ref(), &commit_ref).map_err(|e| {
            CallToolError::invalid_arguments(TOOL, Some(e.detail()))
        })?;
        let bundle = PlasmCompBundle::new(committed.artifact.clone()).map_err(|e| {
            CallToolError::from_message(format!("invalid committed comp: {e}"))
        })?;
        let dry = dry_for_committed_plasm_run(es.as_ref(), &bundle, &committed).map_err(|e| {
            CallToolError::from_message(format!("plan rehydrate failed: {}", e.detail()))
        })?;
        let ux_ctx = PlanUxBuildContext {
            session: Some(es.as_ref()),
            param_bindings: &[],
        };
        let plan_ux = plan_ux_reflection_value(&dry, &ux_ctx);
        let comp = serde_json::to_value(&committed.artifact.comp).map_err(|e| {
            CallToolError::from_message(format!("comp serialize failed: {e}"))
        })?;
        Ok(crate::mcp_ui_payload::ui_read_plan_tool_result(comp, plan_ux))
    }

    pub(crate) async fn handle_ui_read_run(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &Value,
    ) -> Result<CallToolResult, CallToolError> {
        const TOOL: &str = "plasm_ui_read_run";
        let session_ref = parse_logical_session_ref_arg(TOOL, v)?;
        let logical_uuid =
            self.resolve_logical_session_ref_to_uuid(TOOL, &session_ref)?;
        let (_, lookup_arg) = parse_read_run_artifact_lookup(TOOL, v)?;
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
        let doc: Value = serde_json::from_slice(&payload.bytes).map_err(|e| {
            CallToolError::from_message(format!("run snapshot JSON invalid: {e}"))
        })?;
        let entities = doc
            .get("entities")
            .or_else(|| doc.get("results"))
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        let row_count = entities.as_array().map_or(0, |a| a.len());
        let return_label = doc
            .get("return_label")
            .and_then(|v| v.as_str())
            .unwrap_or("result")
            .to_string();
        let run_id = resolved
            .run_id
            .as_ref()
            .map(|r| r.to_wire())
            .unwrap_or_default();
        Ok(crate::mcp_ui_payload::ui_read_run_tool_result(
            run_id.as_str(),
            return_label.as_str(),
            row_count,
            entities,
        ))
    }
}
