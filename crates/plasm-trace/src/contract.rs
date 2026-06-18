//! Canonical wire strings for trace segments (agent, sink, UI).

/// `CodePlanExecute.execution_phase` when live execute begins.
pub const CODE_PLAN_EXECUTION_STARTED: &str = "started";
/// `CodePlanExecute.execution_phase` when live execute finishes successfully.
pub const CODE_PLAN_EXECUTION_COMPLETED: &str = "completed";
/// `CodePlanExecute.execution_phase` when live execute fails after `started`.
pub const CODE_PLAN_EXECUTION_FAILED: &str = "failed";

/// MCP `resources/read` URI query key for read attribution.
pub const MCP_RESOURCE_READ_SOURCE_QUERY_KEY: &str = "plasm.read_source";
/// `McpResourceRead.read_source` when Run Explorer hydrates via MCP.
pub const MCP_RESOURCE_READ_SOURCE_RUN_EXPLORER_UI: &str = "run_explorer_ui";

/// Whether this execute phase should increment `code_plans_executed` KPIs.
#[must_use]
pub fn code_plan_execution_phase_counts_as_executed(phase: &str) -> bool {
    phase == CODE_PLAN_EXECUTION_COMPLETED
}

/// Route MCP resource read char weight to UI vs agent totals.
#[must_use]
pub fn mcp_resource_read_chars_bucket(read_source: Option<&str>) -> McpResourceReadCharsBucket {
    match read_source {
        Some(MCP_RESOURCE_READ_SOURCE_RUN_EXPLORER_UI) => McpResourceReadCharsBucket::Ui,
        _ => McpResourceReadCharsBucket::Agent,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpResourceReadCharsBucket {
    Agent,
    Ui,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_phase_kpi_semantics() {
        assert!(!code_plan_execution_phase_counts_as_executed(
            CODE_PLAN_EXECUTION_STARTED
        ));
        assert!(!code_plan_execution_phase_counts_as_executed(
            CODE_PLAN_EXECUTION_FAILED
        ));
        assert!(code_plan_execution_phase_counts_as_executed(
            CODE_PLAN_EXECUTION_COMPLETED
        ));
    }
}
