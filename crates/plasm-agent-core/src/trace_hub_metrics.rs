//! OpenTelemetry metrics for [`crate::trace_hub::TraceHub`] bounded in-memory queues and the
//! durable ingest channel (see [`crate::trace_hub::TraceHubConfig`] / [`crate::trace_hub::TraceHubBounds`]).

use std::sync::OnceLock;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;

struct TraceHubInstruments {
    completed_queue_depth: Histogram<u64>,
    active_mcp_sessions_with_trace: Histogram<u64>,
    completed_cap_evictions_total: Counter<u64>,
    ingest_enqueue_failed_total: Counter<u64>,
    ingest_enqueued_total: Counter<u64>,
    ingest_dequeued_total: Counter<u64>,
    ingest_queue_backlog: Histogram<u64>,
    ingest_queue_wait_ms: Histogram<u64>,
    ingest_send_wait_ms: Histogram<u64>,
    emit_dropped_no_active_total: Counter<u64>,
}

static TRACE_HUB_INSTRUMENTS: OnceLock<TraceHubInstruments> = OnceLock::new();

fn instruments() -> &'static TraceHubInstruments {
    TRACE_HUB_INSTRUMENTS.get_or_init(|| {
        let meter = global::meter("plasm");
        TraceHubInstruments {
            completed_queue_depth: meter
                .u64_histogram("plasm.trace_hub.completed_queue_depth")
                .with_description(
                    "Samples of completed MCP trace count in TraceHub after each mutation.",
                )
                .build(),
            active_mcp_sessions_with_trace: meter
                .u64_histogram("plasm.trace_hub.active_mcp_sessions_with_trace")
                .with_description(
                    "Samples of active MCP transport sessions holding an in-memory trace after mutations.",
                )
                .build(),
            completed_cap_evictions_total: meter
                .u64_counter("plasm.trace_hub.completed_trace_cap_evictions_total")
                .with_description(
                    "Completed traces evicted from the front of the deque when the completed cap was reached.",
                )
                .build(),
            ingest_enqueue_failed_total: meter
                .u64_counter("plasm.trace_hub.ingest_enqueue_failed_total")
                .with_description(
                    "Trace durable ingest jobs lost because the ingest channel closed (shutdown). Bounded capacity applies backpressure at MCP/HTTP emit time via blocking send, not drops.",
                )
                .build(),
            ingest_enqueued_total: meter
                .u64_counter("plasm.trace_hub.ingest_enqueued_total")
                .with_description(
                    "Trace durable ingest jobs accepted into the bounded ingest channel.",
                )
                .build(),
            ingest_dequeued_total: meter
                .u64_counter("plasm.trace_hub.ingest_dequeued_total")
                .with_description(
                    "Trace durable ingest jobs received from the bounded ingest channel by the worker.",
                )
                .build(),
            ingest_queue_backlog: meter
                .u64_histogram("plasm.trace_hub.ingest_queue_backlog")
                .with_description(
                    "Observed durable ingest backlog (reserved counter) after a successful enqueue.",
                )
                .build(),
            ingest_queue_wait_ms: meter
                .u64_histogram("plasm.trace_hub.ingest_queue_wait_ms")
                .with_description(
                    "Wall time from enqueue to worker receive for durable trace ingest jobs (bounded mpsc only; excludes nested HTTP post latency from spawn_emit_mcp_trace_segment).",
                )
                .build(),
            ingest_send_wait_ms: meter
                .u64_histogram("plasm.trace_hub.ingest_send_wait_ms")
                .with_description(
                    "Time blocked in tokio mpsc send() while waiting for channel capacity (backpressure on MCP/HTTP trace emit path). SSE patches are emitted before this wait.",
                )
                .build(),
            emit_dropped_no_active_total: meter
                .u64_counter("plasm.trace_hub.emit_dropped_no_active_total")
                .with_description(
                    "Trace segment emits dropped because the MCP logical session had no active or resumable completed trace.",
                )
                .build(),
        }
    })
}

fn ingest_attrs(queue_cap: i64) -> [KeyValue; 1] {
    [KeyValue::new("plasm.trace_hub.ingest_queue_cap", queue_cap)]
}

/// Record queue depths after a change to `TraceHubInner::{active,completed}`.
///
/// `oldest_completed_evicted_for_cap` is **true** when the oldest completed trace was dropped to
/// stay within the hub's completed-trace cap.
pub(crate) fn record_trace_hub_queue_state(
    completed_len: usize,
    active_len: usize,
    oldest_completed_evicted_for_cap: bool,
    max_completed: i64,
) {
    let attrs = [KeyValue::new(
        "plasm.trace_hub.max_completed",
        max_completed,
    )];
    let i = instruments();
    i.completed_queue_depth.record(completed_len as u64, &attrs);
    i.active_mcp_sessions_with_trace
        .record(active_len as u64, &attrs);
    if oldest_completed_evicted_for_cap {
        i.completed_cap_evictions_total.add(1, &attrs);
    }
}

/// Record a failure to enqueue a trace segment for durable ingest (`reason`: `closed` when the channel is dropped).
pub(crate) fn record_trace_hub_ingest_enqueue_failed(reason: &'static str, queue_cap: i64) {
    let attrs = [
        KeyValue::new("plasm.trace_hub.ingest_queue_cap", queue_cap),
        KeyValue::new("plasm.trace_hub.ingest_fail_reason", reason),
    ];
    instruments().ingest_enqueue_failed_total.add(1, &attrs);
}

/// Successful enqueue: counters + backlog sample (see `ingest_channel_backlog` on [`crate::trace_hub::TraceHub`]).
pub(crate) fn record_trace_hub_ingest_accepted(backlog_after: u64, queue_cap: i64) {
    let ia = ingest_attrs(queue_cap);
    let i = instruments();
    i.ingest_enqueued_total.add(1, &ia);
    i.ingest_queue_backlog.record(backlog_after, &ia);
}

pub(crate) fn record_trace_hub_ingest_dequeued(queue_cap: i64) {
    let ia = ingest_attrs(queue_cap);
    let i = instruments();
    i.ingest_dequeued_total.add(1, &ia);
}

pub(crate) fn record_trace_hub_ingest_queue_wait_ms(wait_ms: u64, queue_cap: i64) {
    instruments()
        .ingest_queue_wait_ms
        .record(wait_ms, &ingest_attrs(queue_cap));
}

pub(crate) fn record_trace_hub_ingest_send_wait_ms(wait_ms: u64, queue_cap: i64) {
    instruments()
        .ingest_send_wait_ms
        .record(wait_ms, &ingest_attrs(queue_cap));
}

pub(crate) fn record_trace_emit_dropped_no_active() {
    instruments().emit_dropped_no_active_total.add(1, &[]);
}
