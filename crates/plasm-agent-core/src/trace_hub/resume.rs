//! Resume completed MCP traces back into the active map (shared by tool entry and late emit).

use uuid::Uuid;

use super::state::{ActiveTrace, CompletedTrace, TraceHubInner};
use super::TraceHub;

/// Bounds passed to [`ensure_active_locked`] (avoids repeating six hub config fields).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResumeLockedParams {
    pub timeline_cap: usize,
    pub sse_broadcast_capacity: usize,
    pub last_activity_ms: u64,
    pub max_completed_traces: i64,
}

impl TraceHub {
    pub(super) fn resume_locked_params(&self, last_activity_ms: u64) -> ResumeLockedParams {
        let b = self.config.bounds;
        ResumeLockedParams {
            timeline_cap: b.max_timeline_events,
            sse_broadcast_capacity: b.sse_broadcast_capacity,
            last_activity_ms,
            max_completed_traces: b.max_completed_traces as i64,
        }
    }
}

/// Match policy when locating a row in [`TraceHubInner::completed`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompletedResumeCriteria<'a> {
    pub trace_id: Option<Uuid>,
    pub tenant_id: Option<&'a str>,
}

impl<'a> CompletedResumeCriteria<'a> {
    /// Late segment emit: same logical session key only (tenant/trace validated on the row).
    pub(crate) fn for_emit() -> CompletedResumeCriteria<'static> {
        CompletedResumeCriteria {
            trace_id: None,
            tenant_id: None,
        }
    }

    pub(crate) fn strict(trace_id: Uuid, tenant_id: &'a str) -> Self {
        CompletedResumeCriteria {
            trace_id: Some(trace_id),
            tenant_id: Some(tenant_id),
        }
    }
}

fn completed_matches(
    c: &CompletedTrace,
    session_trace_key: &str,
    criteria: CompletedResumeCriteria<'_>,
) -> bool {
    if c.session_trace_key != session_trace_key {
        return false;
    }
    if let Some(want_id) = criteria.trace_id {
        if c.trace_id != want_id {
            return false;
        }
    }
    if let Some(want_tenant) = criteria.tenant_id {
        if c.meta.tenant_id != want_tenant {
            return false;
        }
    }
    true
}

pub(crate) fn find_completed_index(
    completed: &std::collections::VecDeque<CompletedTrace>,
    session_trace_key: &str,
    criteria: CompletedResumeCriteria<'_>,
) -> Option<usize> {
    completed
        .iter()
        .position(|c| completed_matches(c, session_trace_key, criteria))
}

pub(crate) fn active_from_completed(
    completed: CompletedTrace,
    timeline_cap: usize,
    last_activity_ms: u64,
) -> ActiveTrace {
    let mut data = completed.data;
    data.timeline_max_events = timeline_cap;
    ActiveTrace {
        trace_id: completed.trace_id,
        session_trace_key: completed.session_trace_key,
        logical_session_id: completed.logical_session_id,
        mcp_transport_session_id: completed.mcp_transport_session_id,
        meta: completed.meta,
        data,
        started_ms: completed.started_ms,
        last_activity_ms,
        seq: completed.last_seq_emitted,
    }
}

/// Resume a completed trace into `active` when absent. Returns true if active row exists afterward.
pub(crate) fn ensure_active_locked(
    g: &mut TraceHubInner,
    session_trace_key: &str,
    criteria: CompletedResumeCriteria<'_>,
    params: ResumeLockedParams,
) -> bool {
    if g.active.contains_key(session_trace_key) {
        return true;
    }
    let Some(pos) = find_completed_index(&g.completed, session_trace_key, criteria) else {
        return false;
    };
    let completed = g.completed.remove(pos).expect("position just found");
    let trace_id = completed.trace_id;
    let active = active_from_completed(completed, params.timeline_cap, params.last_activity_ms);
    let _tx = TraceHub::broadcast_tx(g, trace_id, params.sse_broadcast_capacity);
    g.active.insert(session_trace_key.to_string(), active);
    crate::trace_hub_metrics::record_trace_hub_queue_state(
        g.completed.len(),
        g.active.len(),
        false,
        params.max_completed_traces,
    );
    true
}
