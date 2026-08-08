//! Wall-clock envelope for each segment (SSE patches + durable replay ordering).

use serde::{Deserialize, Serialize};

use crate::TraceSegment;

/// Historical payload key nested by older agents beside a flattened [`TraceEvent`].
/// New emits never write this; decode strips it so lake rows remain readable.
const LEGACY_PLASM_AUDIT_PAYLOAD_KEY: &str = "_plasm_audit";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceEvent {
    pub emitted_at_ms: u64,
    #[serde(flatten)]
    pub segment: TraceSegment,
}

impl TraceEvent {
    pub fn at(emitted_at_ms: u64, segment: TraceSegment) -> Self {
        Self {
            emitted_at_ms,
            segment,
        }
    }

    /// Decode durable `payload_json` as a [`TraceEvent`].
    ///
    /// Strips the legacy `_plasm_audit` nest (correlation now lives on
    /// [`plasm_trace_wire::AuditEvent::logical_session_id`]).
    pub fn from_payload_json(mut payload: serde_json::Value) -> Result<Self, String> {
        if let serde_json::Value::Object(ref mut map) = payload {
            map.remove(LEGACY_PLASM_AUDIT_PAYLOAD_KEY);
        }
        serde_json::from_value(payload).map_err(|e| format!("TraceEvent deserialize: {e}"))
    }

    /// Serialize for durable projection / HTTP detail records.
    pub fn to_detail_record_value(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|e| format!("TraceEvent serialize: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{minimal_trace_comp_json, TraceCompWire, TraceSegment};

    fn minimal_eval() -> TraceEvent {
        let comp = Arc::new(
            TraceCompWire::from_json_value(minimal_trace_comp_json()).expect("minimal comp"),
        );
        TraceEvent::at(
            42,
            TraceSegment::CodePlanEvaluate {
                plan_handle: "p1".into(),
                plan_id: "00000000-0000-0000-0000-000000000000".into(),
                plan_name: "demo".into(),
                plan_hash: "abc".into(),
                plan_uri: String::new(),
                canonical_plan_uri: String::new(),
                plan_http_path: String::new(),
                prompt_hash: "p".repeat(64),
                session_id: "s1".into(),
                node_count: 1,
                code_chars: 10,
                comp,
                plan_ux_reflection: Some(serde_json::json!({
                    "schema_version": 3,
                    "flow": {
                        "schema_version": 1,
                        "verdict": "clean",
                        "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
                        "violations": [],
                        "trace": []
                    }
                })),
            },
        )
    }

    #[test]
    fn from_payload_json_round_trips_code_plan() {
        let ev = minimal_eval();
        let payload = serde_json::to_value(&ev).expect("ser");
        let decoded = TraceEvent::from_payload_json(payload).expect("decode");
        assert_eq!(decoded.emitted_at_ms, 42);
        assert!(matches!(
            decoded.segment,
            TraceSegment::CodePlanEvaluate { .. }
        ));
        let record = decoded.to_detail_record_value().expect("ser");
        assert_eq!(
            record.get("kind").and_then(|k| k.as_str()),
            Some("code_plan_evaluate")
        );
        assert!(record.get(LEGACY_PLASM_AUDIT_PAYLOAD_KEY).is_none());
    }

    #[test]
    fn from_payload_json_strips_legacy_plasm_audit_nest() {
        let ev = minimal_eval();
        let mut payload = serde_json::to_value(&ev).expect("ser");
        if let serde_json::Value::Object(ref mut map) = payload {
            map.insert(
                LEGACY_PLASM_AUDIT_PAYLOAD_KEY.into(),
                serde_json::json!({ "logical_session_id": "ls-1" }),
            );
        }
        let decoded = TraceEvent::from_payload_json(payload).expect("decode strips nest");
        assert_eq!(decoded.emitted_at_ms, 42);
    }
}
