//! Canonical trace model shared by the agent in-memory trace hub and durable trace sink
//! projections: one JSON shape for live SSE, HTTP detail, and Iceberg replay.

mod contract;
mod event;
mod segment;
mod segment_counters;
mod session;
mod totals;

pub use contract::{
    code_plan_execution_phase_counts_as_executed, mcp_resource_read_chars_bucket,
    McpResourceReadCharsBucket, CODE_PLAN_EXECUTION_COMPLETED, CODE_PLAN_EXECUTION_FAILED,
    CODE_PLAN_EXECUTION_STARTED, MCP_RESOURCE_READ_SOURCE_QUERY_KEY,
    MCP_RESOURCE_READ_SOURCE_RUN_EXPLORER_UI,
};

pub use event::TraceEvent;
pub use plasm_observability_contracts::RunArtifactArchiveRef;
pub use segment::{CodePlanRunArtifactRef, PlasmLineTraceMeta, TraceSegment};
pub use session::{
    session_data_from_events, session_data_from_ordered_events, SessionTraceCountersSnapshot,
    SessionTraceData, DEFAULT_TRACE_TIMELINE_MAX_EVENTS,
};
pub use totals::{merge_trace_totals, totals_from_session_data, TraceTotals};
