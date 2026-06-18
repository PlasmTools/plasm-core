//! Canonical trace model shared by the agent in-memory trace hub and durable trace sink
//! projections: one JSON shape for live SSE, HTTP detail, and Iceberg replay.

mod event;
mod segment;
mod session;
mod totals;

pub use event::TraceEvent;
pub use plasm_observability_contracts::RunArtifactArchiveRef;
pub use segment::{CodePlanRunArtifactRef, PlasmLineTraceMeta, TraceSegment};
pub use session::{
    session_data_from_events, session_data_from_ordered_events, SessionTraceCountersSnapshot,
    SessionTraceData, DEFAULT_TRACE_TIMELINE_MAX_EVENTS,
};
pub use totals::{merge_trace_totals, totals_from_session_data, TraceTotals};
