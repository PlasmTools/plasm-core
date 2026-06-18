//! Shared counter increments for [`TraceSegment`] kinds (session aggregates + legacy totals fold).

use crate::{
    code_plan_execution_phase_counts_as_executed, mcp_resource_read_chars_bucket,
    McpResourceReadCharsBucket,
};

pub fn apply_mcp_resource_read_counters(
    agent_chars: &mut u64,
    ui_chars: &mut u64,
    total_duration_ms: &mut u64,
    chars_added: u64,
    duration_ms: u64,
    read_source: Option<&str>,
) {
    match mcp_resource_read_chars_bucket(read_source) {
        McpResourceReadCharsBucket::Ui => {
            *ui_chars = ui_chars.saturating_add(chars_added);
        }
        McpResourceReadCharsBucket::Agent => {
            *agent_chars = agent_chars.saturating_add(chars_added);
        }
    }
    *total_duration_ms = total_duration_ms.saturating_add(duration_ms);
}

pub fn apply_code_plan_evaluate_counters(
    code_plans_evaluated: &mut u64,
    code_plan_code_chars: &mut u64,
    code_plan_nodes: &mut u64,
    node_count: usize,
    code_chars: u64,
) {
    *code_plans_evaluated = code_plans_evaluated.saturating_add(1);
    *code_plan_code_chars = code_plan_code_chars.saturating_add(code_chars);
    *code_plan_nodes = code_plan_nodes.saturating_add(node_count as u64);
}

/// Returns whether `code_plans_executed` was incremented (completed phase only).
pub fn apply_code_plan_execute_counters(
    code_plans_executed: &mut u64,
    code_plan_code_chars: &mut u64,
    code_plan_nodes: &mut u64,
    code_plan_derived_runs: &mut u64,
    execution_phase: &str,
    node_count: usize,
    code_chars: u64,
    run_ids_len: usize,
    run_artifacts_len: usize,
) -> bool {
    if !code_plan_execution_phase_counts_as_executed(execution_phase) {
        return false;
    }
    *code_plans_executed = code_plans_executed.saturating_add(1);
    *code_plan_code_chars = code_plan_code_chars.saturating_add(code_chars);
    *code_plan_nodes = code_plan_nodes.saturating_add(node_count as u64);
    *code_plan_derived_runs = code_plan_derived_runs.saturating_add(
        run_artifacts_len.max(run_ids_len) as u64,
    );
    true
}
