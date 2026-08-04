//! Single decode path for `mcp_trace_segment` audit payloads.
//!
//! `payload` is a pure [`TraceEvent`] JSON object. Logical-session correlation lives on
//! [`AuditEvent::logical_session_id`], not nested beside the flattened segment.

use plasm_trace::TraceEvent;

use crate::metrics::record_trace_event_deserialize_failed;
use crate::model::AuditEvent;

/// Decode a raw payload object as [`TraceEvent`].
pub(crate) fn decode_audit_payload(payload: serde_json::Value) -> Result<TraceEvent, String> {
    TraceEvent::from_payload_json(payload)
}

/// Decode an `mcp_trace_segment` row; on failure increment the deserialize metric.
///
/// When `warn_message` is `Some`, emit a structured warn (fixed target — tracing requires a
/// literal `target:`).
pub(crate) fn decode_mcp_trace_segment(
    ev: &AuditEvent,
    warn_message: Option<&'static str>,
) -> Option<TraceEvent> {
    match decode_audit_payload(ev.payload.clone()) {
        Ok(event) => Some(event),
        Err(err) => {
            let kind_hint = segment_kind_hint(&ev.payload);
            record_trace_event_deserialize_failed(kind_hint);
            if let Some(message) = warn_message {
                tracing::warn!(
                    target: "plasm_trace_sink::decode",
                    error = %err,
                    trace_id = %ev.trace_id,
                    event_id = %ev.event_id,
                    segment_kind = kind_hint,
                    "{message}"
                );
            }
            None
        }
    }
}

fn segment_kind_hint(payload: &serde_json::Value) -> &str {
    payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}
