//! Aggregate KPIs for trace list cards (demo-oriented).

use serde::{Deserialize, Serialize};

use crate::{SessionTraceData, TraceSegment};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraceTotals {
    pub plasm_tool_calls: u64,
    pub plasm_expressions: u64,
    pub expression_lines: u64,
    pub multi_line_plasm_invocations: u64,
    pub teaching_prompt_chars: u64,
    pub plasm_invocation_chars: u64,
    pub plasm_response_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_chars: u64,
    pub total_duration_ms: u64,
    pub network_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub http_trace_entry_count: u64,
    #[serde(default)]
    pub code_plans_evaluated: u64,
    #[serde(default)]
    pub code_plans_executed: u64,
    #[serde(default)]
    pub code_plan_code_chars: u64,
    #[serde(default)]
    pub code_plan_nodes: u64,
    #[serde(default)]
    pub code_plan_derived_runs: u64,
}

pub fn totals_from_session_data(data: &SessionTraceData) -> TraceTotals {
    // Prefer cumulative aggregates (complete session) when present.
    if data.aggregate_expression_lines > 0 || data.aggregate_plasm_expressions > 0 {
        return TraceTotals {
            plasm_tool_calls: data.plasm_call_count,
            plasm_expressions: data.aggregate_plasm_expressions,
            expression_lines: data.aggregate_expression_lines,
            multi_line_plasm_invocations: data.aggregate_multi_line_plasm_invocations,
            teaching_prompt_chars: data.teaching_prompt_chars,
            plasm_invocation_chars: data.plasm_invocation_chars,
            plasm_response_chars: data.plasm_response_chars,
            mcp_resource_read_chars: data.mcp_resource_read_chars,
            total_duration_ms: data.aggregate_total_duration_ms,
            network_requests: data.aggregate_network_requests,
            cache_hits: data.aggregate_cache_hits,
            cache_misses: data.aggregate_cache_misses,
            http_trace_entry_count: data.aggregate_http_trace_entry_count,
            code_plans_evaluated: data.code_plans_evaluated,
            code_plans_executed: data.code_plans_executed,
            code_plan_code_chars: data.code_plan_code_chars,
            code_plan_nodes: data.code_plan_nodes,
            code_plan_derived_runs: data.code_plan_derived_runs,
        };
    }

    // Legacy: older persisted traces without aggregate_* — fold the retained record window only.
    let mut t = TraceTotals {
        plasm_tool_calls: data.plasm_call_count,
        teaching_prompt_chars: data.teaching_prompt_chars,
        plasm_invocation_chars: data.plasm_invocation_chars,
        plasm_response_chars: data.plasm_response_chars,
        mcp_resource_read_chars: data.mcp_resource_read_chars,
        code_plans_evaluated: data.code_plans_evaluated,
        code_plans_executed: data.code_plans_executed,
        code_plan_code_chars: data.code_plan_code_chars,
        code_plan_nodes: data.code_plan_nodes,
        code_plan_derived_runs: data.code_plan_derived_runs,
        ..Default::default()
    };
    for ev in data.records.iter() {
        match &ev.segment {
            TraceSegment::PlasmInvocation {
                multi_line,
                expression_count,
                ..
            } => {
                t.plasm_expressions = t.plasm_expressions.saturating_add(*expression_count as u64);
                if *multi_line {
                    t.multi_line_plasm_invocations =
                        t.multi_line_plasm_invocations.saturating_add(1);
                }
            }
            TraceSegment::PlasmLine {
                duration_ms,
                stats,
                http_calls,
                ..
            } => {
                t.expression_lines = t.expression_lines.saturating_add(1);
                t.total_duration_ms = t.total_duration_ms.saturating_add(*duration_ms);
                t.network_requests = t
                    .network_requests
                    .saturating_add(stats.network_requests as u64);
                t.cache_hits = t.cache_hits.saturating_add(stats.cache_hits as u64);
                t.cache_misses = t.cache_misses.saturating_add(stats.cache_misses as u64);
                t.http_trace_entry_count = t
                    .http_trace_entry_count
                    .saturating_add(http_calls.len() as u64);
            }
            TraceSegment::McpResourceRead { chars_added, .. } => {
                t.mcp_resource_read_chars = t.mcp_resource_read_chars.saturating_add(*chars_added);
            }
            TraceSegment::CodePlanEvaluate {
                node_count,
                code_chars,
                ..
            } => {
                t.code_plans_evaluated = t.code_plans_evaluated.saturating_add(1);
                t.code_plan_code_chars = t.code_plan_code_chars.saturating_add(*code_chars);
                t.code_plan_nodes = t.code_plan_nodes.saturating_add(*node_count as u64);
            }
            TraceSegment::CodePlanExecute {
                node_count,
                code_chars,
                run_ids,
                run_artifacts,
                ..
            } => {
                t.code_plans_executed = t.code_plans_executed.saturating_add(1);
                t.code_plan_code_chars = t.code_plan_code_chars.saturating_add(*code_chars);
                t.code_plan_nodes = t.code_plan_nodes.saturating_add(*node_count as u64);
                t.code_plan_derived_runs = t
                    .code_plan_derived_runs
                    .saturating_add(run_artifacts.len().max(run_ids.len()) as u64);
            }
            _ => {}
        }
    }
    t
}

impl From<TraceTotals> for plasm_observability_contracts::TraceTotals {
    fn from(t: TraceTotals) -> Self {
        Self {
            plasm_tool_calls: t.plasm_tool_calls,
            plasm_expressions: t.plasm_expressions,
            expression_lines: t.expression_lines,
            multi_line_plasm_invocations: t.multi_line_plasm_invocations,
            teaching_prompt_chars: t.teaching_prompt_chars,
            plasm_invocation_chars: t.plasm_invocation_chars,
            plasm_response_chars: t.plasm_response_chars,
            mcp_resource_read_chars: t.mcp_resource_read_chars,
            total_duration_ms: t.total_duration_ms,
            network_requests: t.network_requests,
            cache_hits: t.cache_hits,
            cache_misses: t.cache_misses,
            http_trace_entry_count: t.http_trace_entry_count,
            code_plans_evaluated: t.code_plans_evaluated,
            code_plans_executed: t.code_plans_executed,
            code_plan_code_chars: t.code_plan_code_chars,
            code_plan_nodes: t.code_plan_nodes,
            code_plan_derived_runs: t.code_plan_derived_runs,
        }
    }
}
