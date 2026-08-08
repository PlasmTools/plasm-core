//! Wire/serde DTOs for the **execution-trace** lane (ingest, list/detail, run artifact identity).
//!
//! This is **not** OpenTelemetry. OTLP bootstrap lives in `plasm-otel`. Session timeline segments
//! live in `plasm-trace`. Optional durable Iceberg persistence is the SaaS ops binary
//! `plasm-trace-sink` (product-only row types stay there).

mod model;
mod run_artifact;

pub use model::{
    AuditEvent, DurableTraceDetail, IngestBatchRequest, IngestBatchResponse, TraceDetailRecord,
    TraceDetailResponse, TraceGetResponse, TraceListResponse, TraceSummary, TraceTotals,
    AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT, SCHEMA_VERSION,
};
pub use run_artifact::RunArtifactArchiveRef;
