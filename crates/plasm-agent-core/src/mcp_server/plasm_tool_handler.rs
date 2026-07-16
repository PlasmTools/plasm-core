//! Shared MCP `plasm` / `plasm_run` tool handler (`handle_plasm_mcp_tool`).

use std::sync::Arc;
use std::time::Instant;

use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolResult, ContentBlock, TextContent};
use rust_mcp_sdk::McpServer;
use tracing::Instrument;

use crate::http_execute::try_dispatch_operation_program;
use crate::incoming_auth::tenant_scope;
use crate::session_identity::LogicalSessionId;
use crate::trace_hub::{McpPlasmTraceSink, PlanRunTraceHooks};
use crate::trace_sink_emit::PlasmTraceContext;

use super::committed_plasm_run;
use super::mcp_plasm_invoke::{parse_mcp_plasm_invocation, McpPlasmInvocation};
use super::plasm_tool_dry_run;
use super::tool_parse::parse_logical_session_ref_arg;
use super::transport::plasm_invocation_char_count;
use super::{PlasmMcpHandler, MCP_EXECUTE_SESSION_UNAVAILABLE};

impl PlasmMcpHandler {
    /// Shared MCP implementation for [`Self::handle_call_tool_request`] (`plasm` = plan-only, `plasm_run` = execute).
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn handle_plasm_mcp_tool(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
        tool_name: &'static str,
        dry_run_only: bool,
        started: Instant,
    ) -> Result<CallToolResult, CallToolError> {
        let principal_incoming = self.ensure_mcp_principal(key, runtime).await?;
        let session_ref = parse_logical_session_ref_arg(tool_name, v)?;
        let logical_uuid = self.resolve_logical_session_ref_to_uuid(tool_name, &session_ref)?;
        let scope = tenant_scope(principal_incoming.as_ref());
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(LogicalSessionId(logical_uuid), &scope)
            .await
        {
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                "logical_session_ref is unknown or does not belong to this tenant scope",
            )));
        }
        let ls_key = logical_uuid.to_string();
        let state = self.logical_mutex(key, &ls_key).await;
        let needs_binding_hydrate = {
            let g = state.lock().await;
            g.binding.is_none()
        };
        if needs_binding_hydrate {
            if let Some(b) = self.resolve_binding_for_logical(key, logical_uuid).await {
                let mut g = state.lock().await;
                g.binding = Some(b);
                drop(g);
                self.persist_transport_state(key).await;
            }
        }
        let invocation = match parse_mcp_plasm_invocation(tool_name, v, dry_run_only) {
            Ok(invocation) => invocation,
            Err(result) => {
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "error",
                    "invalid_arguments",
                    started.elapsed(),
                );
                return Ok(result);
            }
        };
        let reasoning = v
            .get("reasoning")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty());
        let wait_live = true;
        let force_run = false;
        let plasm_tool_span = if dry_run_only {
            crate::spans::mcp_tool_plasm(false, 1, session_ref.as_str())
        } else {
            crate::spans::mcp_tool_plasm_run(false, 1, session_ref.as_str())
        };
        let run_live = matches!(invocation, McpPlasmInvocation::Run(_));
        let (binding, this_invocation_chars, meta_index) = {
            let g = state.lock().await;
            let binding = g.binding.clone();
            let this_invocation_chars =
                plasm_invocation_char_count(invocation.invocation_text(), reasoning);
            let meta_index = Arc::clone(&g.meta_index);
            drop(g);
            let mut g = state.lock().await;
            g.stats.plasm_invocation_chars = g
                .stats
                .plasm_invocation_chars
                .saturating_add(this_invocation_chars);
            g.stats.plasm_call_count = g.stats.plasm_call_count.saturating_add(1);
            (binding, this_invocation_chars, meta_index)
        };
        let Some(b) = binding else {
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "no_session",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                "No session: call `plasm_context` with capability picks (`seeds`) first.",
            )));
        };

        if self
            .plasm
            .get_execute_session(&b.prompt_hash, &b.session_id)
            .await
            .is_none()
        {
            {
                let mut g = state.lock().await;
                g.binding = None;
            }
            {
                self.plasm
                    .logical_execute_bindings
                    .remove(&logical_uuid)
                    .await;
            }
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "session_expired",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                MCP_EXECUTE_SESSION_UNAVAILABLE,
            )));
        }
        let trace_meta = self.trace_session_meta(key, runtime).await;
        let trace_id = self
            .plasm
            .trace_hub
            .ensure_logical_session(&ls_key, Some(key), trace_meta)
            .await;
        let reasoning_chars = reasoning.map(|r| r.chars().count() as u64);
        let call_index = self
            .plasm
            .trace_hub
            .trace_record_plasm_invocation(
                &ls_key,
                false,
                1,
                reasoning_chars,
                this_invocation_chars,
                reasoning.map(str::to_string),
            )
            .await;
        let mcp_trace = PlasmTraceContext {
            trace_id,
            call_index: Some(call_index as i64),
            mcp_session_id: Some(key.to_string()),
            logical_session_id: Some(ls_key.clone()),
            logical_session_ref: Some(session_ref.clone()),
        };

        let sink = McpPlasmTraceSink {
            hub: Arc::clone(&self.plasm.trace_hub),
            mcp_key: ls_key.clone(),
            call_index,
        };
        let artifact_mode = self.resolved_artifact_access_mode(key, runtime).await;
        let ui_apps_enabled = self.mcp_ui_apps_enabled_for_runtime(runtime).await;
        let delivery_profile =
            crate::mcp_delivery::McpDeliveryProfile::resolve(ui_apps_enabled, artifact_mode);
        let mcp_result_policy = crate::mcp_run_markdown::McpResultTransportPolicy {
            artifact_access: artifact_mode,
            ..Default::default()
        };
        let plan_trace = PlanRunTraceHooks {
            trace: mcp_trace.clone(),
            sink,
            meta_index: Some(meta_index),
        };

        let run_result = async {
            let Some(es) = self
                .plasm
                .get_execute_session(&b.prompt_hash, &b.session_id)
                .await
            else {
                return Err(MCP_EXECUTE_SESSION_UNAVAILABLE.to_string());
            };
            if let Some(program) = invocation.program() {
                if let Some(op_result) = try_dispatch_operation_program(
                    &es,
                    Some(self.plasm.as_ref()),
                    Some(&mcp_trace),
                    program,
                    Some(self.plasm.sessions.symbol_map_cross_cache()),
                )
                .await
                {
                    return op_result;
                }
            }
            if run_live {
                let run_target = invocation
                    .run_target()
                    .ok_or_else(|| "missing `run_ref` on plasm_run invocation".to_string())?;
                let ingress = committed_plasm_run::resolve_mcp_live_run_ingress(
                    &es,
                    &mcp_trace,
                    run_target,
                    self.plasm.engine.prompt_pipeline(),
                    self.plasm.sessions.symbol_map_cross_cache(),
                    call_index,
                )
                .await?;
                let wire = committed_plasm_run::McpExecuteWire {
                    prompt_hash: b.prompt_hash.clone(),
                    session_id: b.session_id.clone(),
                    session_ref: session_ref.clone(),
                    ls_key: ls_key.clone(),
                    mcp_session_key: key.to_string(),
                };
                let artifacts = committed_plasm_run::CommittedRunArtifacts {
                    trace_hub: Arc::clone(&self.plasm.trace_hub),
                    run_artifacts: Arc::clone(&self.plasm.run_artifacts),
                    program_for_trace: ingress.program_for_trace.clone(),
                    plan_call_index: call_index,
                };
                committed_plasm_run::execute_mcp_live_run(committed_plasm_run::ExecuteMcpLiveRun {
                    es: Arc::clone(&es),
                    host: Arc::clone(&self.plasm),
                    wire,
                    bundle: ingress.bundle,
                    kind: ingress.kind,
                    mcp_trace: mcp_trace.clone(),
                    artifacts,
                    plan_trace: Some(plan_trace),
                    mcp_result_policy: Some(mcp_result_policy),
                    force_run,
                    wait_live,
                })
                .await
            } else {
                let program = invocation
                    .program()
                    .ok_or_else(|| "missing `program`: call `plasm` with a program".to_string())?;
                plasm_tool_dry_run::execute_plasm_tool_dry_run(
                    plasm_tool_dry_run::PlasmDryRunContext {
                        host: Arc::clone(&self.plasm),
                        es: Arc::clone(&es),
                        binding: &b,
                        session_ref: session_ref.as_str(),
                        ls_key: ls_key.as_str(),
                        call_index,
                        mcp_session_key: key,
                        mcp_trace,
                        plan_trace,
                        mcp_result_policy,
                    },
                    program,
                )
                .await
            }
        }
        .instrument(plasm_tool_span)
        .await;
        match run_result {
            Ok(out) => {
                let markdown = out
                    .run_markdown
                    .unwrap_or_else(|| "# Plasm program plan\n\nNo execution output.".to_string());
                let response_chars = markdown.chars().count() as u64;
                if response_chars > 0 {
                    let mut g = state.lock().await;
                    g.stats.plasm_response_chars =
                        g.stats.plasm_response_chars.saturating_add(response_chars);
                    self.plasm
                        .trace_hub
                        .trace_note_plasm_response_chars(
                            &ls_key,
                            response_chars,
                            tool_name,
                            call_index,
                            false,
                            1,
                        )
                        .await;
                }
                let (tok_prompt, tok_inv, tok_resp, tok_total) =
                    self.mcp_plasm_token_snapshot_logical(key, &ls_key).await;
                tracing::info!(
                    target: "plasm_agent::mcp",
                    tool = tool_name,
                    ok = true,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    "MCP tool: plasm / plasm_run"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "success",
                    "none",
                    started.elapsed(),
                );
                let res = if let Some(m) = out.run_plasm_meta {
                    if matches!(
                        delivery_profile,
                        crate::mcp_delivery::McpDeliveryProfile::ToolFallback
                    ) {
                        tracing::debug!(
                            target: "plasm_agent::mcp",
                            ?artifact_mode,
                            "omit structuredContent.ui for tool-only MCP host"
                        );
                    }
                    crate::mcp_ui_payload::DualLaneToolResult::from_tool_meta(
                        markdown,
                        m,
                        delivery_profile,
                        out.inline_plan_ui,
                    )
                    .into_call_tool_result()
                } else {
                    CallToolResult::from_content(vec![ContentBlock::TextContent(TextContent::new(
                        markdown, None, None,
                    ))])
                };
                self.schedule_persist_transport_state(key);
                Ok(res)
            }
            Err(msg) => {
                self.plasm
                    .trace_hub
                    .trace_add_plasm_error(&ls_key, call_index, None, msg.clone())
                    .await;
                let (tok_prompt, tok_inv, tok_resp, tok_total) =
                    self.mcp_plasm_token_snapshot_logical(key, &ls_key).await;
                tracing::error!(
                    target: "plasm_agent::mcp",
                    tool = tool_name,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    message = %msg,
                    "MCP tool: plasm / plasm_run failed"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "error",
                    "execute_failed",
                    started.elapsed(),
                );
                Ok(CallToolResult::with_error(CallToolError::from_message(msg)))
            }
        }
    }
}
