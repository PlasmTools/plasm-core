//! Token-minimal async operation progress: wire lines, short `_meta.plasm.op`, coalesced emits.

use crate::operation::{OperationPhase, OperationProgress};
use crate::plan_dry_display::PlanDryVerdict;
use plasm_core::{OperationHandle, PlanCommitRef};
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const OP_PROGRESS_COALESCE: Duration = Duration::from_secs(2);

/// Structured live-run stats for MCP App progress (Run Explorer telemetry header).
#[derive(Debug, Clone, Copy, Default)]
pub struct OpNotifyStats {
    pub calls: Option<u64>,
    pub last_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub rows: Option<u64>,
}

/// MCP push payload queued for the transport session reporter loop.
#[derive(Debug, Clone)]
pub struct McpOpPending {
    pub transport_key: String,
    pub line: String,
    pub n: u64,
    pub plan_commit: Option<String>,
    pub stats: OpNotifyStats,
}

/// Global queue for MCP `notifications/plasm/op` (drained by MCP session reporter).
#[derive(Default)]
pub struct OperationProgressHub {
    pending_mcp: Mutex<Vec<McpOpPending>>,
}

impl OperationProgressHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn queue_mcp_notify(
        &self,
        transport_key: &str,
        line: &str,
        n: u64,
        plan_commit: Option<&PlanCommitRef>,
        stats: OpNotifyStats,
    ) {
        self.pending_mcp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(McpOpPending {
                transport_key: transport_key.to_string(),
                line: line.to_string(),
                n,
                plan_commit: plan_commit.map(|c| c.as_str().to_string()),
                stats,
            });
    }

    pub fn drain_mcp_pending(&self) -> Vec<McpOpPending> {
        std::mem::take(&mut *self.pending_mcp.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpWireSig {
    Accept,
    Running,
    Unchanged,
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpAgentSnapshot {
    pub phase: OperationPhase,
    pub step: u32,
    pub rows: u64,
}

impl OpAgentSnapshot {
    pub fn from_running(progress: &OperationProgress) -> Self {
        Self {
            phase: OperationPhase::Running,
            step: progress.step,
            rows: progress.rows_materialized,
        }
    }

    pub fn terminal(phase: OperationPhase, rows: u64) -> Self {
        Self {
            phase,
            step: 0,
            rows,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperationAgentEmitState {
    pub seq: u64,
    pub last_emitted: OpAgentSnapshot,
    pub last_emit_at: Instant,
    pub last_line: String,
}

impl Default for OperationAgentEmitState {
    fn default() -> Self {
        Self {
            seq: 0,
            last_emitted: OpAgentSnapshot {
                phase: OperationPhase::Running,
                step: 0,
                rows: 0,
            },
            last_emit_at: Instant::now(),
            last_line: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpProgressEvent {
    pub seq: u64,
    pub line: String,
    pub terminal: bool,
}

#[must_use]
pub fn should_emit_agent_progress(
    prev: OpAgentSnapshot,
    next: OpAgentSnapshot,
    last_emit_at: Instant,
) -> bool {
    if prev.phase != next.phase || prev.step != next.step {
        return true;
    }
    next.phase == OperationPhase::Running
        && next.rows > prev.rows
        && last_emit_at.elapsed() >= OP_PROGRESS_COALESCE
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum McpEmitCoalesce {
    /// Accept line: always emit with seq 1.
    Accept,
    /// Running: emit on step/row change per [`should_emit_agent_progress`].
    OnChange,
    /// Running: ~1s wall clock or on change.
    Coalesced,
    /// Terminal done/failed/cancelled: always emit.
    Terminal,
}

/// Queue one MCP progress wire line for bounded sync (shared by accept/running/terminal).
#[allow(clippy::too_many_arguments)]
pub fn try_queue_mcp_sync_progress_line(
    hub: &OperationProgressHub,
    stats: OpNotifyStats,
    transport_key: &str,
    plan_commit: Option<&PlanCommitRef>,
    handle: &OperationHandle,
    emit_state: &mut OperationAgentEmitState,
    sig: OpWireSig,
    progress: Option<&OperationProgress>,
    dry_verdict: Option<PlanDryVerdict>,
    error: Option<&str>,
    coalesce: McpEmitCoalesce,
) -> bool {
    let default_progress = OperationProgress::default();
    let progress_ref = progress.unwrap_or(&default_progress);

    match coalesce {
        McpEmitCoalesce::Accept => {
            emit_state.seq = 1;
            emit_state.last_emit_at = Instant::now();
        }
        McpEmitCoalesce::Terminal => {
            emit_state.seq = emit_state.seq.saturating_add(1);
            emit_state.last_emit_at = Instant::now();
        }
        McpEmitCoalesce::OnChange => {
            if sig != OpWireSig::Running {
                return false;
            }
            let snapshot = OpAgentSnapshot::from_running(progress_ref);
            if !should_emit_agent_progress(
                emit_state.last_emitted,
                snapshot,
                emit_state.last_emit_at,
            ) {
                return false;
            }
            emit_state.seq = emit_state.seq.saturating_add(1);
            emit_state.last_emitted = snapshot;
            emit_state.last_emit_at = Instant::now();
        }
        McpEmitCoalesce::Coalesced => {
            if sig != OpWireSig::Running {
                return false;
            }
            let snapshot = OpAgentSnapshot::from_running(progress_ref);
            let time_due = emit_state.last_emit_at.elapsed() >= Duration::from_secs(1);
            if !time_due
                && !should_emit_agent_progress(
                    emit_state.last_emitted,
                    snapshot,
                    emit_state.last_emit_at,
                )
            {
                return false;
            }
            emit_state.seq = emit_state.seq.saturating_add(1);
            emit_state.last_emitted = snapshot;
            emit_state.last_emit_at = Instant::now();
        }
    }

    let line = render_op_wire_line(
        handle,
        sig,
        progress,
        if coalesce == McpEmitCoalesce::Accept {
            plan_commit
        } else {
            None
        },
        if coalesce == McpEmitCoalesce::Accept {
            dry_verdict
        } else {
            None
        },
        error,
    );
    emit_state.last_line = line.clone();
    let seq = emit_state.seq;
    hub.queue_mcp_notify(transport_key, &line, seq, plan_commit, stats);
    true
}

pub fn render_op_wire_line(
    handle: &OperationHandle,
    sig: OpWireSig,
    progress: Option<&OperationProgress>,
    plan_commit: Option<&PlanCommitRef>,
    dry_verdict: Option<PlanDryVerdict>,
    error: Option<&str>,
) -> String {
    let h = handle.as_str();
    match sig {
        OpWireSig::Accept => {
            let mut parts = vec![format!("`{h}` +")];
            if let Some(pc) = plan_commit {
                parts.push(pc.as_str().to_string());
            }
            if dry_verdict == Some(PlanDryVerdict::Review) {
                parts.push("review".to_string());
            }
            parts.join(" ")
        }
        OpWireSig::Running => {
            let default_progress = OperationProgress::default();
            let p = progress.unwrap_or(&default_progress);
            let mut out = format!("`{h}` ~");
            if p.step_total > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!(" {}/{}", p.step.max(1), p.step_total),
                );
            }
            if let Some(ref l) = p.label {
                out.push(' ');
                out.push_str(l);
            }
            if p.rows_materialized > 0 {
                let _ =
                    std::fmt::Write::write_fmt(&mut out, format_args!(" {}r", p.rows_materialized));
            }
            out
        }
        OpWireSig::Unchanged => {
            let default_progress = OperationProgress::default();
            let p = progress.unwrap_or(&default_progress);
            let mut out = format!("`{h}` =");
            if p.step_total > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!(" {}/{}", p.step.max(1), p.step_total),
                );
            }
            if let Some(ref l) = p.label {
                out.push(' ');
                out.push_str(l);
            }
            if p.rows_materialized > 0 {
                let _ =
                    std::fmt::Write::write_fmt(&mut out, format_args!(" {}r", p.rows_materialized));
            }
            out
        }
        OpWireSig::Done => {
            let rows = progress.map(|p| p.rows_materialized).unwrap_or(0);
            if rows > 0 {
                format!("`{h}` ! {rows}r")
            } else {
                format!("`{h}` !")
            }
        }
        OpWireSig::Cancelled => {
            let rows = progress.map(|p| p.rows_materialized).unwrap_or(0);
            if rows > 0 {
                format!("`{h}` x {rows}r")
            } else {
                format!("`{h}` x")
            }
        }
        OpWireSig::Failed => {
            let msg = error.unwrap_or("failed");
            format!("`{h}` ? {msg}")
        }
    }
}

pub fn render_op_wire_markdown(line: &str) -> String {
    format!("```text\n{line}\n```")
}

pub fn async_poll_accept_markdown_suffix(handle: &OperationHandle) -> String {
    format!(
        "\n\n_Operation `{}` is running. MCP `plasm_run` awaits server-side; do not poll._",
        handle.as_str()
    )
}

pub fn async_poll_unchanged_markdown_suffix(handle: &OperationHandle) -> String {
    format!(
        "\n\n_Operation `{}` is still open. MCP `plasm_run` awaits server-side; do not poll._",
        handle.as_str()
    )
}

pub fn async_poll_progress_markdown_suffix(handle: &OperationHandle) -> String {
    format!(
        "\n\n_Operation `{}` progressed. MCP `plasm_run` awaits server-side; do not poll._",
        handle.as_str()
    )
}

pub fn op_plasm_meta_short(
    handle: &OperationHandle,
    sig: OpWireSig,
    seq: u64,
    progress: Option<&OperationProgress>,
    plan_commit: Option<&PlanCommitRef>,
) -> Map<String, Value> {
    let phase = match sig {
        OpWireSig::Accept | OpWireSig::Running | OpWireSig::Unchanged => "running",
        OpWireSig::Done => "succeeded",
        OpWireSig::Cancelled => "cancelled",
        OpWireSig::Failed => "failed",
    };
    let mut plasm = Map::new();
    let mut continuity = Map::new();
    continuity.insert("p".into(), json!(phase));
    continuity.insert("h".into(), json!(handle.as_str()));
    plasm.insert("continuity".into(), Value::Object(continuity));

    let mut op = Map::new();
    op.insert("n".into(), json!(seq));
    match sig {
        OpWireSig::Accept => {
            op.insert("+".into(), json!(1));
            if let Some(pc) = plan_commit {
                op.insert("c".into(), json!(pc.as_str()));
            }
        }
        OpWireSig::Running => {
            op.insert("~".into(), json!(1));
            if let Some(p) = progress {
                if p.step_total > 0 {
                    op.insert(
                        "s".into(),
                        json!(format!("{}/{}", p.step.max(1), p.step_total)),
                    );
                }
                if let Some(ref l) = p.label {
                    op.insert("l".into(), json!(l));
                }
                if p.rows_materialized > 0 {
                    op.insert("r".into(), json!(p.rows_materialized));
                }
            }
        }
        OpWireSig::Unchanged => {
            op.insert("=".into(), json!(1));
            if let Some(p) = progress {
                if p.step_total > 0 {
                    op.insert(
                        "s".into(),
                        json!(format!("{}/{}", p.step.max(1), p.step_total)),
                    );
                }
                if p.rows_materialized > 0 {
                    op.insert("r".into(), json!(p.rows_materialized));
                }
            }
        }
        OpWireSig::Done => {
            op.insert("!".into(), json!(1));
            if let Some(p) = progress {
                if p.rows_materialized > 0 {
                    op.insert("r".into(), json!(p.rows_materialized));
                }
            }
        }
        OpWireSig::Cancelled => {
            op.insert("x".into(), json!(1));
            if let Some(p) = progress {
                if p.rows_materialized > 0 {
                    op.insert("r".into(), json!(p.rows_materialized));
                }
            }
        }
        OpWireSig::Failed => {
            op.insert("?".into(), json!(1));
        }
    }
    plasm.insert("op".into(), Value::Object(op));
    plasm
}

pub fn op_poll_unchanged_meta(
    seq: u64,
    progress: Option<&OperationProgress>,
) -> Map<String, Value> {
    let mut plasm = Map::new();
    let mut op = Map::new();
    op.insert("n".into(), json!(seq));
    op.insert("=".into(), json!(1));
    if let Some(p) = progress {
        if p.step_total > 0 {
            op.insert(
                "s".into(),
                json!(format!("{}/{}", p.step.max(1), p.step_total)),
            );
        }
        if p.rows_materialized > 0 {
            op.insert("r".into(), json!(p.rows_materialized));
        }
    }
    plasm.insert("op".into(), Value::Object(op));
    plasm
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::OperationHandle;

    #[test]
    fn async_poll_discipline_mcp_line_matches_include() {
        const MCP_LINE: &str = include_str!("mcp_prompt/async_poll_discipline_mcp.txt");
        assert!(MCP_LINE.contains("awaits server-side"));
        assert!(MCP_LINE.contains("Do not poll"));
    }

    #[test]
    fn op_poll_unchanged_meta_includes_step_and_rows() {
        let prog = OperationProgress {
            step: 2,
            step_total: 5,
            label: Some("r1".into()),
            rows_materialized: 42,
        };
        let meta = op_poll_unchanged_meta(7, Some(&prog));
        let op = meta
            .get("op")
            .and_then(|v| v.as_object())
            .expect("op object");
        assert_eq!(op.get("n").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(op.get("=").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(op.get("s").and_then(|v| v.as_str()), Some("2/5"));
        assert_eq!(op.get("r").and_then(|v| v.as_u64()), Some(42));
    }

    #[test]
    fn mcp_pending_carries_notify_stats() {
        let hub = OperationProgressHub::new();
        hub.queue_mcp_notify(
            "tk1",
            "`o1` ~ 1/2 items",
            3,
            Some(&PlanCommitRef::mint(0)),
            OpNotifyStats {
                calls: Some(5),
                last_ms: Some(120),
                elapsed_ms: Some(4_500),
                rows: Some(10),
            },
        );
        let pending = hub.drain_mcp_pending();
        assert_eq!(pending.len(), 1);
        let p = &pending[0];
        assert_eq!(p.stats.calls, Some(5));
        assert_eq!(p.stats.last_ms, Some(120));
        assert_eq!(p.stats.elapsed_ms, Some(4_500));
        assert_eq!(p.stats.rows, Some(10));
    }

    #[test]
    fn wire_line_all_sigs() {
        let h = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 1);
        assert!(render_op_wire_line(
            &h,
            OpWireSig::Accept,
            None,
            Some(&PlanCommitRef::mint(0)),
            Some(PlanDryVerdict::Review),
            None
        )
        .contains("+"));
        let prog = OperationProgress {
            step: 2,
            step_total: 5,
            label: Some("r1".into()),
            rows_materialized: 42,
        };
        assert_eq!(
            render_op_wire_line(&h, OpWireSig::Running, Some(&prog), None, None, None),
            "`l_AAAAAAAAQACAAAAAAAAAAQ_o1` ~ 2/5 r1 42r"
        );
        assert_eq!(
            render_op_wire_line(&h, OpWireSig::Unchanged, Some(&prog), None, None, None),
            "`l_AAAAAAAAQACAAAAAAAAAAQ_o1` = 2/5 r1 42r"
        );
        assert_eq!(
            render_op_wire_line(&h, OpWireSig::Done, Some(&prog), None, None, None),
            "`l_AAAAAAAAQACAAAAAAAAAAQ_o1` ! 42r"
        );
    }

    #[test]
    fn coalesce_same_step_rows() {
        let prev = OpAgentSnapshot::from_running(&OperationProgress {
            step: 1,
            step_total: 3,
            label: None,
            rows_materialized: 10,
        });
        let next = OpAgentSnapshot::from_running(&OperationProgress {
            step: 1,
            step_total: 3,
            label: None,
            rows_materialized: 20,
        });
        assert!(!should_emit_agent_progress(prev, next, Instant::now()));
    }

    #[test]
    fn coalesce_step_change_always_emits() {
        let prev = OpAgentSnapshot::from_running(&OperationProgress {
            step: 1,
            step_total: 3,
            label: None,
            rows_materialized: 10,
        });
        let next = OpAgentSnapshot::from_running(&OperationProgress {
            step: 2,
            step_total: 3,
            label: None,
            rows_materialized: 10,
        });
        assert!(should_emit_agent_progress(prev, next, Instant::now()));
    }
}
