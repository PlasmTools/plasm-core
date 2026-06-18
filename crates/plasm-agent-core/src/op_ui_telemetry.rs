//! Canonical Run Explorer / MCP App live-progress wire shape (HTTP JSON + SSE).

use plasm_core::OperationHandle;
use serde::{Deserialize, Serialize};

use crate::execute_session::ExecuteSession;
use crate::mcp_transport_store::persisted_operations::{
    PersistedOperationDescriptor, PersistedOperationPhase,
};
use crate::operation::{OperationPhase, OperationProgress};
use crate::operation_progress::{
    render_op_wire_line, OpNotifyStats, OpProgressEvent, OpWireSig,
};

/// Stable progress telemetry for HTTP JSON, SSE, and MCP App host context.
///
/// **Persisted / cross-replica path:** `calls`, `last_ms`, and `elapsed_ms` stay unset — only
/// in-process live runs attach [`OpNotifyStats`] from [`ExecuteSession::live_run_notify_stats`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OpUiTelemetry {
    pub line: String,
    pub n: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

impl OpUiTelemetry {
    pub fn json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_progress_event(ev: &OpProgressEvent) -> Self {
        Self {
            line: ev.line.clone(),
            n: ev.seq,
            calls: ev.stats.calls,
            last_ms: ev.stats.last_ms,
            elapsed_ms: ev.stats.elapsed_ms,
            rows: ev.stats.rows,
            terminal: ev.terminal,
        }
    }

    pub fn from_live(sess: &ExecuteSession, handle: &OperationHandle) -> Option<Self> {
        let op = sess.get_operation(handle)?;
        let rows = (op.progress.rows_materialized > 0).then_some(op.progress.rows_materialized);
        let stats = sess.live_run_notify_stats(rows);
        let sig = if op.phase == OperationPhase::Running {
            OpWireSig::Running
        } else {
            OpWireSig::Done
        };
        let line = if op.agent_emit.last_line.is_empty() {
            render_op_wire_line(
                handle,
                sig,
                Some(&op.progress),
                op.plan_commit_ref.as_ref(),
                op.dry_verdict,
                None,
            )
        } else {
            op.agent_emit.last_line.clone()
        };
        Some(Self {
            line,
            n: op.agent_emit.seq,
            calls: stats.calls,
            last_ms: stats.last_ms,
            elapsed_ms: stats.elapsed_ms,
            rows: stats.rows.or(rows),
            terminal: op.phase != OperationPhase::Running,
        })
    }

    pub fn from_persisted(desc: &PersistedOperationDescriptor, handle: &OperationHandle) -> Self {
        let progress: OperationProgress = desc.progress.clone().into();
        let line = if desc.agent_last_line.is_empty() {
            let sig = if desc.phase == PersistedOperationPhase::Running {
                OpWireSig::Running
            } else {
                OpWireSig::Done
            };
            render_op_wire_line(handle, sig, Some(&progress), None, None, None)
        } else {
            desc.agent_last_line.clone()
        };
        Self {
            line,
            n: desc.agent_seq,
            rows: (progress.rows_materialized > 0).then_some(progress.rows_materialized),
            terminal: desc.phase.is_terminal(),
            ..Default::default()
        }
    }

    pub fn apply_notify_stats(&mut self, stats: OpNotifyStats) {
        if stats.calls.is_some() {
            self.calls = stats.calls;
        }
        if stats.last_ms.is_some() {
            self.last_ms = stats.last_ms;
        }
        if stats.elapsed_ms.is_some() {
            self.elapsed_ms = stats.elapsed_ms;
        }
        if stats.rows.is_some() {
            self.rows = stats.rows;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_snapshot_omits_live_run_stats() {
        let handle = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 1);
        let desc = PersistedOperationDescriptor {
            handle: handle.as_str().to_string(),
            phase: PersistedOperationPhase::Running,
            progress: Default::default(),
            started_at_unix: 0,
            error: None,
            run_artifact_id: None,
            plan_commit_ref: Some("pc0".into()),
            dry_verdict: None,
            display_map: Default::default(),
            agent_seq: 2,
            agent_last_line: "`l_AAAAAAAAQACAAAAAAAAAAQ_o1` ~ 1/2".into(),
        };
        let snap = OpUiTelemetry::from_persisted(&desc, &handle);
        assert_eq!(snap.line, "`l_AAAAAAAAQACAAAAAAAAAAQ_o1` ~ 1/2");
        assert!(snap.calls.is_none());
        assert!(snap.last_ms.is_none());
        assert!(snap.elapsed_ms.is_none());
    }

    #[test]
    fn progress_event_carries_stats_on_wire() {
        let ev = OpProgressEvent {
            seq: 5,
            line: "line".into(),
            terminal: false,
            stats: OpNotifyStats {
                calls: Some(2),
                last_ms: Some(100),
                elapsed_ms: Some(3000),
                rows: Some(10),
            },
        };
        let snap = OpUiTelemetry::from_progress_event(&ev);
        assert_eq!(snap.n, 5);
        assert_eq!(snap.calls, Some(2));
        assert_eq!(snap.elapsed_ms, Some(3000));
    }

    #[test]
    fn golden_wire_vector_round_trip() {
        let raw = include_str!("../../../fixtures/run_explorer/op_ui_telemetry_wire.json");
        let parsed: OpUiTelemetry = serde_json::from_str(raw).expect("fixture parse");
        let again: OpUiTelemetry = serde_json::from_str(&parsed.json_line()).expect("re-parse");
        assert_eq!(parsed, again);
        assert_eq!(parsed.calls, Some(3));
        assert!(!parsed.terminal);
    }
}
