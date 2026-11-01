//! MCP `tools/call` name dispatch (keeps [`ServerHandler`] futures small).
//!
//! Heavy tools (`plasm_context`, `plasm`, `plasm_run`, discovery) run on
//! [`LivePlanRunPool`](crate::live_plan_run_worker::LivePlanRunPool) so debug teaching /
//! plan stacks do not overflow default tokio worker threads. Prefer that over
//! process-wide `RUST_MIN_STACK`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mcp_server::request::McpRequest;
use crate::mcp_server::request::ToolFailure;
use rmcp::model::{CallToolRequestParams, CallToolResult};

use super::resource_helpers::mcp_call_tool_error_class;
use super::schema::args_value;
use super::{mcp_key, PlasmMcpHandler};
use tracing::Instrument;

pub(crate) fn dispatch_plasm_mcp_call_tool_request<'a>(
    handler: &'a PlasmMcpHandler,
    params: CallToolRequestParams,
    runtime: Arc<McpRequest>,
) -> futures_util::future::BoxFuture<'a, Result<CallToolResult, ToolFailure>> {
    let tool_name = params.name.clone();
    Box::pin(
        dispatch_plasm_mcp_call_tool_request_inner(handler, params, runtime)
            .instrument(crate::spans::mcp_call_tool(tool_name.as_ref())),
    )
}

/// Run an owned MCP tool future on a dedicated large-stack admission pool.
/// Its permits must be separate from execution: a tool can await a live run.
///
/// The tool future is created and polled only on the worker (`run_local`) to
/// contain its stack requirements. Errors are folded into
/// [`crate::mcp_server::request::tool_error_result`] before the `Send` result crosses back.
async fn on_live_plan_stack<F, Fut>(f: F) -> CallToolResult
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<CallToolResult, ToolFailure>> + 'static,
{
    static TOOL_POOL: std::sync::OnceLock<crate::live_plan_run_worker::LivePlanRunPool> =
        std::sync::OnceLock::new();
    let pool = TOOL_POOL.get_or_init(crate::live_plan_run_worker::LivePlanRunPool::new);
    match pool
        .run_local(move || async move {
            match f().await {
                Ok(r) => r,
                Err(e) => crate::mcp_server::request::tool_error_result(e),
            }
        })
        .await
    {
        Ok(result) => result,
        Err(e) => {
            crate::mcp_server::request::tool_error_result(ToolFailure::rejected(e.to_string()))
        }
    }
}

async fn dispatch_plasm_mcp_call_tool_request_inner(
    handler: &PlasmMcpHandler,
    params: CallToolRequestParams,
    runtime: Arc<McpRequest>,
) -> Result<CallToolResult, ToolFailure> {
    fn record_app_ui_tool(
        tname: &'static str,
        res: &Result<CallToolResult, ToolFailure>,
        started: Instant,
    ) {
        record_named_mcp_tool(tname, res, started);
    }

    fn record_named_mcp_tool(
        tname: &'static str,
        res: &Result<CallToolResult, ToolFailure>,
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

    match params.name.as_ref() {
        "plasm_review" | "plasm_example" => {
            let started = Instant::now();
            let owned = handler.clone();
            let name = params.name.to_string();
            let metric = if name == "plasm_review" {
                "plasm_review"
            } else {
                "plasm_example"
            };
            let result = on_live_plan_stack(move || async move {
                owned.handle_learning_tool(&key, &runtime, &v, &name).await
            })
            .await;
            record_tool_result(metric, &result, started);
            Ok(result)
        }
        "plasm_context" => {
            let started = Instant::now();
            let owned = handler.clone();
            let key = key.clone();
            let runtime = Arc::clone(&runtime);
            let v = v.clone();
            let res = on_live_plan_stack(move || {
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
        "open_workflow" | "dry_workflow" | "run_workflow" => {
            let started = Instant::now();
            let owned = handler.clone();
            let name = params.name.to_string();
            let metric = match name.as_str() {
                "open_workflow" => "open_workflow",
                "dry_workflow" => "dry_workflow",
                _ => "run_workflow",
            };
            let result = on_live_plan_stack(move || async move {
                match name.as_str() {
                    "open_workflow" => {
                        owned
                            .handle_mcp_tool_open_workflow(&key, &runtime, &v)
                            .await
                    }
                    "dry_workflow" => owned.handle_mcp_tool_dry_workflow(&key, &runtime, &v).await,
                    _ => owned.handle_mcp_tool_run_workflow(&key, &runtime, &v).await,
                }
            })
            .await;
            record_tool_result(metric, &result, started);
            Ok(result)
        }
        "plasm" | "plasm_run" => {
            let started = Instant::now();
            let dry_run_only = matches!(params.name.as_ref(), "plasm");
            let tool_name: &'static str = if dry_run_only { "plasm" } else { "plasm_run" };
            let owned = handler.clone();
            let key = key.clone();
            let runtime = Arc::clone(&runtime);
            let v = v.clone();
            let res = on_live_plan_stack(move || {
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
            Err(ToolFailure::unknown_tool(params.name.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_admission_can_await_live_execution_without_sharing_permits() {
        let execution_pool = Arc::new(crate::live_plan_run_worker::LivePlanRunPool::new());
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            on_live_plan_stack(move || async move {
                execution_pool
                    .run_local(|| async { CallToolResult::success(Vec::new()) })
                    .await
                    .map_err(|error| ToolFailure::rejected(error.to_string()))
            }),
        )
        .await
        .expect("tool admission must not occupy a live execution permit");
        assert!(!result.is_error.unwrap_or(false));
    }
}
