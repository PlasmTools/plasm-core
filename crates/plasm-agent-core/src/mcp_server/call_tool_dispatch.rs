//! MCP `tools/call` name dispatch (keeps [`ServerHandler`] futures small).
//!
//! Heavy tools (`plasm_context`, `plasm`, `plasm_run`, discovery) run on
//! [`LivePlanRunPool`](crate::live_plan_run_worker::LivePlanRunPool) so debug teaching /
//! plan stacks do not overflow default tokio worker threads. Prefer that over
//! process-wide `RUST_MIN_STACK`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolRequestParams, CallToolResult};
use rust_mcp_sdk::McpServer;

use super::resource_helpers::mcp_call_tool_error_class;
use super::schema::args_value;
use super::{mcp_key, PlasmMcpHandler};
use tracing::Instrument;

pub(crate) async fn dispatch_plasm_mcp_call_tool_request(
    handler: &PlasmMcpHandler,
    params: CallToolRequestParams,
    runtime: Arc<dyn McpServer>,
) -> Result<CallToolResult, CallToolError> {
    let tool_name = params.name.clone();
    Box::pin(
        dispatch_plasm_mcp_call_tool_request_inner(handler, params, runtime)
            .instrument(crate::spans::mcp_call_tool(tool_name.as_str())),
    )
    .await
}

/// Run an owned MCP tool future on the live-plan large-stack worker pool.
///
/// [`CallToolError`] is not `Send` (`Box<dyn Error>`), so the tool future is created and
/// polled only on the worker (`run_local`), and errors are folded into
/// [`CallToolResult::with_error`] before the `Send` result crosses back.
async fn on_live_plan_stack<F, Fut>(handler: &PlasmMcpHandler, f: F) -> CallToolResult
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<CallToolResult, CallToolError>> + 'static,
{
    let pool = handler.plasm.live_plan_pool();
    match pool
        .run_local(move || async move {
            Ok(match f().await {
                Ok(r) => r,
                Err(e) => CallToolResult::with_error(e),
            })
        })
        .await
    {
        Ok(result) => result,
        Err(e) => CallToolResult::with_error(CallToolError::from_message(e)),
    }
}

async fn dispatch_plasm_mcp_call_tool_request_inner(
    handler: &PlasmMcpHandler,
    params: CallToolRequestParams,
    runtime: Arc<dyn McpServer>,
) -> Result<CallToolResult, CallToolError> {
    fn record_workflow_tool(
        tname: &'static str,
        res: &Result<CallToolResult, CallToolError>,
        started: Instant,
    ) {
        record_named_mcp_tool(tname, res, started);
    }

    fn record_app_ui_tool(
        tname: &'static str,
        res: &Result<CallToolResult, CallToolError>,
        started: Instant,
    ) {
        record_named_mcp_tool(tname, res, started);
    }

    fn record_named_mcp_tool(
        tname: &'static str,
        res: &Result<CallToolResult, CallToolError>,
        started: Instant,
    ) {
        let elapsed = started.elapsed();
        match res {
            Ok(_) => crate::metrics::record_mcp_tool(tname, None, "success", "none", elapsed),
            Err(e) => crate::metrics::record_mcp_tool(
                tname,
                None,
                "error",
                mcp_call_tool_error_class(e),
                elapsed,
            ),
        }
    }

    fn record_tool_result(tname: &'static str, res: &CallToolResult, started: Instant) {
        let elapsed = started.elapsed();
        let status = if res.is_error.unwrap_or(false) {
            "error"
        } else {
            "success"
        };
        crate::metrics::record_mcp_tool(tname, None, status, "none", elapsed);
    }

    let key = mcp_key(&runtime)?;
    let v = args_value(&params);

    tracing::trace!(
        target: "plasm_agent.mcp.call_tool",
        tool = %params.name,
        "call_tool dispatch"
    );

    match params.name.as_str() {
        "plasm_context" => {
            let started = Instant::now();
            let owned = handler.clone();
            let key = key.clone();
            let runtime = Arc::clone(&runtime);
            let v = v.clone();
            let res = on_live_plan_stack(handler, move || {
                let owned = owned.clone();
                async move {
                    owned
                        .handle_mcp_tool_plasm_context(key.as_str(), &runtime, &v)
                        .await
                }
            })
            .await;
            record_tool_result("plasm_context", &res, started);
            Ok(res)
        }
        "plasm_ui_list_catalogs" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_ui_list_catalogs(key.as_str(), &runtime)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(
                    "plasm_ui_list_catalogs",
                    None,
                    "success",
                    "none",
                    elapsed,
                ),
                Err(e) => crate::metrics::record_mcp_tool(
                    "plasm_ui_list_catalogs",
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
        }
        "open_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_open_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("open_workflow", &res, started);
            res
        }
        "dry_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_dry_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("dry_workflow", &res, started);
            res
        }
        "run_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_run_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("run_workflow", &res, started);
            res
        }
        "plasm" | "plasm_run" => {
            let started = Instant::now();
            let dry_run_only = matches!(params.name.as_str(), "plasm");
            let tool_name: &'static str = if dry_run_only { "plasm" } else { "plasm_run" };
            let owned = handler.clone();
            let key = key.clone();
            let runtime = Arc::clone(&runtime);
            let v = v.clone();
            let res = on_live_plan_stack(handler, move || {
                let owned = owned.clone();
                async move {
                    owned
                        .handle_plasm_mcp_tool(&key, &runtime, &v, tool_name, dry_run_only, started)
                        .await
                }
            })
            .await;
            record_tool_result(tool_name, &res, started);
            Ok(res)
        }
        "plasm_read_run_artifact" => {
            let started = Instant::now();
            let res = handler
                .handle_read_run_artifact(key.as_str(), &runtime, &v)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(
                    "plasm_read_run_artifact",
                    None,
                    "success",
                    "none",
                    elapsed,
                ),
                Err(e) => crate::metrics::record_mcp_tool(
                    "plasm_read_run_artifact",
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
        }
        "plasm_ui_read_plan" => {
            let started = Instant::now();
            let res = handler
                .handle_ui_read_plan(key.as_str(), &runtime, &v)
                .await;
            record_app_ui_tool("plasm_ui_read_plan", &res, started);
            res
        }
        "plasm_ui_read_run" => {
            let started = Instant::now();
            let res = handler.handle_ui_read_run(key.as_str(), &runtime, &v).await;
            record_app_ui_tool("plasm_ui_read_run", &res, started);
            res
        }
        _ => {
            crate::metrics::record_mcp_tool(
                "unknown_tool",
                None,
                "error",
                "unknown_tool",
                Duration::from_secs(0),
            );
            Err(CallToolError::unknown_tool(params.name.clone()))
        }
    }
}
