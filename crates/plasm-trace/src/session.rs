//! Per-session aggregate counters plus ordered [`TraceEvent`] timeline (bounded window).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::{
    segment_counters::{
        apply_code_plan_evaluate_counters, apply_code_plan_execute_counters,
        apply_mcp_resource_read_counters,
    },
    TraceEvent, TraceSegment,
};

/// Default max events retained in RAM for the live timeline window ([`SessionTraceData::records`]).
pub const DEFAULT_TRACE_TIMELINE_MAX_EVENTS: usize = 4096;

/// Full trace for one MCP transport session or HTTP execute session key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTraceData {
    pub mcp_session_id: String,
    pub teaching_prompt_chars: u64,
    pub plasm_invocation_chars: u64,
    pub plasm_response_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_ui_chars: u64,
    pub plasm_call_count: u64,
    /// Cumulative line-level stats for the whole session (not reduced when the in-memory window drops old events).
    #[serde(default)]
    pub aggregate_plasm_expressions: u64,
    #[serde(default)]
    pub aggregate_multi_line_plasm_invocations: u64,
    #[serde(default)]
    pub aggregate_expression_lines: u64,
    #[serde(default)]
    pub aggregate_total_duration_ms: u64,
    #[serde(default)]
    pub aggregate_network_requests: u64,
    #[serde(default)]
    pub aggregate_cache_hits: u64,
    #[serde(default)]
    pub aggregate_cache_misses: u64,
    #[serde(default)]
    pub aggregate_http_trace_entry_count: u64,
    #[serde(default)]
    pub code_plans_evaluated: u64,
    #[serde(default)]
    pub code_plans_executed: u64,
    #[serde(default)]
    pub code_plan_code_chars: u64,
    #[serde(default)]
    pub code_plan_nodes: u64,
    #[serde(default)]
    pub code_plan_derived_runs: u64,
    /// Max events kept in [`Self::records`]; older events are dropped after totals are updated.
    #[serde(default = "default_timeline_cap")]
    pub timeline_max_events: usize,
    pub records: VecDeque<TraceEvent>,
}

fn default_timeline_cap() -> usize {
    DEFAULT_TRACE_TIMELINE_MAX_EVENTS
}

/// Serializable counters-only snapshot of [`SessionTraceData`] (no timeline), for durable trace
/// head rows and incremental aggregation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionTraceCountersSnapshot {
    pub teaching_prompt_chars: u64,
    pub plasm_invocation_chars: u64,
    pub plasm_response_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_ui_chars: u64,
    pub plasm_call_count: u64,
    pub aggregate_plasm_expressions: u64,
    pub aggregate_multi_line_plasm_invocations: u64,
    pub aggregate_expression_lines: u64,
    pub aggregate_total_duration_ms: u64,
    pub aggregate_network_requests: u64,
    pub aggregate_cache_hits: u64,
    pub aggregate_cache_misses: u64,
    pub aggregate_http_trace_entry_count: u64,
    #[serde(default)]
    pub code_plans_evaluated: u64,
    #[serde(default)]
    pub code_plans_executed: u64,
    #[serde(default)]
    pub code_plan_code_chars: u64,
    #[serde(default)]
    pub code_plan_nodes: u64,
    #[serde(default)]
    pub code_plan_derived_runs: u64,
}

impl From<&SessionTraceData> for SessionTraceCountersSnapshot {
    fn from(d: &SessionTraceData) -> Self {
        Self {
            teaching_prompt_chars: d.teaching_prompt_chars,
            plasm_invocation_chars: d.plasm_invocation_chars,
            plasm_response_chars: d.plasm_response_chars,
            mcp_resource_read_chars: d.mcp_resource_read_chars,
            mcp_resource_read_ui_chars: d.mcp_resource_read_ui_chars,
            plasm_call_count: d.plasm_call_count,
            aggregate_plasm_expressions: d.aggregate_plasm_expressions,
            aggregate_multi_line_plasm_invocations: d.aggregate_multi_line_plasm_invocations,
            aggregate_expression_lines: d.aggregate_expression_lines,
            aggregate_total_duration_ms: d.aggregate_total_duration_ms,
            aggregate_network_requests: d.aggregate_network_requests,
            aggregate_cache_hits: d.aggregate_cache_hits,
            aggregate_cache_misses: d.aggregate_cache_misses,
            aggregate_http_trace_entry_count: d.aggregate_http_trace_entry_count,
            code_plans_evaluated: d.code_plans_evaluated,
            code_plans_executed: d.code_plans_executed,
            code_plan_code_chars: d.code_plan_code_chars,
            code_plan_nodes: d.code_plan_nodes,
            code_plan_derived_runs: d.code_plan_derived_runs,
        }
    }
}

impl SessionTraceCountersSnapshot {
    /// Rebuild a [`SessionTraceData`] shell with the same counters (empty timeline).
    pub fn into_session_data(self, mcp_session_id: impl Into<String>) -> SessionTraceData {
        SessionTraceData {
            mcp_session_id: mcp_session_id.into(),
            teaching_prompt_chars: self.teaching_prompt_chars,
            plasm_invocation_chars: self.plasm_invocation_chars,
            plasm_response_chars: self.plasm_response_chars,
            mcp_resource_read_chars: self.mcp_resource_read_chars,
            mcp_resource_read_ui_chars: self.mcp_resource_read_ui_chars,
            plasm_call_count: self.plasm_call_count,
            aggregate_plasm_expressions: self.aggregate_plasm_expressions,
            aggregate_multi_line_plasm_invocations: self.aggregate_multi_line_plasm_invocations,
            aggregate_expression_lines: self.aggregate_expression_lines,
            aggregate_total_duration_ms: self.aggregate_total_duration_ms,
            aggregate_network_requests: self.aggregate_network_requests,
            aggregate_cache_hits: self.aggregate_cache_hits,
            aggregate_cache_misses: self.aggregate_cache_misses,
            aggregate_http_trace_entry_count: self.aggregate_http_trace_entry_count,
            code_plans_evaluated: self.code_plans_evaluated,
            code_plans_executed: self.code_plans_executed,
            code_plan_code_chars: self.code_plan_code_chars,
            code_plan_nodes: self.code_plan_nodes,
            code_plan_derived_runs: self.code_plan_derived_runs,
            timeline_max_events: DEFAULT_TRACE_TIMELINE_MAX_EVENTS,
            records: VecDeque::new(),
        }
    }
}

impl Default for SessionTraceData {
    fn default() -> Self {
        Self {
            mcp_session_id: String::new(),
            teaching_prompt_chars: 0,
            plasm_invocation_chars: 0,
            plasm_response_chars: 0,
            mcp_resource_read_chars: 0,
            mcp_resource_read_ui_chars: 0,
            plasm_call_count: 0,
            aggregate_plasm_expressions: 0,
            aggregate_multi_line_plasm_invocations: 0,
            aggregate_expression_lines: 0,
            aggregate_total_duration_ms: 0,
            aggregate_network_requests: 0,
            aggregate_cache_hits: 0,
            aggregate_cache_misses: 0,
            aggregate_http_trace_entry_count: 0,
            code_plans_evaluated: 0,
            code_plans_executed: 0,
            code_plan_code_chars: 0,
            code_plan_nodes: 0,
            code_plan_derived_runs: 0,
            timeline_max_events: DEFAULT_TRACE_TIMELINE_MAX_EVENTS,
            records: VecDeque::new(),
        }
    }
}

impl SessionTraceData {
    pub fn new(mcp_session_id: impl Into<String>) -> Self {
        Self::new_with_timeline_cap(mcp_session_id, DEFAULT_TRACE_TIMELINE_MAX_EVENTS)
    }

    pub fn new_with_timeline_cap(
        mcp_session_id: impl Into<String>,
        timeline_max_events: usize,
    ) -> Self {
        Self {
            mcp_session_id: mcp_session_id.into(),
            timeline_max_events: timeline_max_events.max(1),
            ..Default::default()
        }
    }

    /// Update aggregate counters from one event (no timeline deque). Same semantics as the first
    /// half of [`Self::push_event`]; used by durable trace-head projections.
    pub fn apply_event_counters(&mut self, ev: &TraceEvent) {
        match &ev.segment {
            TraceSegment::PlasmContext {
                teaching_prompt_chars_added,
                ..
            }
            | TraceSegment::ExpandDomain {
                teaching_prompt_chars_added,
                ..
            } => {
                self.teaching_prompt_chars = self
                    .teaching_prompt_chars
                    .saturating_add(*teaching_prompt_chars_added);
            }
            TraceSegment::PlasmInvocation {
                multi_line,
                expression_count,
                plasm_invocation_chars_added,
                ..
            } => {
                self.plasm_invocation_chars = self
                    .plasm_invocation_chars
                    .saturating_add(*plasm_invocation_chars_added);
                self.plasm_call_count = self.plasm_call_count.saturating_add(1);
                self.aggregate_plasm_expressions = self
                    .aggregate_plasm_expressions
                    .saturating_add(*expression_count as u64);
                if *multi_line {
                    self.aggregate_multi_line_plasm_invocations = self
                        .aggregate_multi_line_plasm_invocations
                        .saturating_add(1);
                }
            }
            TraceSegment::TeachingPromptCharsDelta { chars_added } => {
                self.teaching_prompt_chars =
                    self.teaching_prompt_chars.saturating_add(*chars_added);
            }
            TraceSegment::PlasmResponseCharsDelta { chars_added, .. } => {
                self.plasm_response_chars = self.plasm_response_chars.saturating_add(*chars_added);
            }
            TraceSegment::McpResourceRead {
                chars_added,
                duration_ms,
                read_source,
                ..
            } => {
                apply_mcp_resource_read_counters(
                    &mut self.mcp_resource_read_chars,
                    &mut self.mcp_resource_read_ui_chars,
                    &mut self.aggregate_total_duration_ms,
                    *chars_added,
                    *duration_ms,
                    read_source.as_deref(),
                );
            }
            TraceSegment::PlasmLine {
                duration_ms,
                stats,
                http_calls,
                ..
            } => {
                self.aggregate_expression_lines = self.aggregate_expression_lines.saturating_add(1);
                self.aggregate_total_duration_ms = self
                    .aggregate_total_duration_ms
                    .saturating_add(*duration_ms);
                self.aggregate_network_requests = self
                    .aggregate_network_requests
                    .saturating_add(stats.network_requests as u64);
                self.aggregate_cache_hits = self
                    .aggregate_cache_hits
                    .saturating_add(stats.cache_hits as u64);
                self.aggregate_cache_misses = self
                    .aggregate_cache_misses
                    .saturating_add(stats.cache_misses as u64);
                self.aggregate_http_trace_entry_count = self
                    .aggregate_http_trace_entry_count
                    .saturating_add(http_calls.len() as u64);
            }
            TraceSegment::CodePlanEvaluate {
                node_count,
                code_chars,
                ..
            } => {
                apply_code_plan_evaluate_counters(
                    &mut self.code_plans_evaluated,
                    &mut self.code_plan_code_chars,
                    &mut self.code_plan_nodes,
                    *node_count,
                    *code_chars,
                );
            }
            TraceSegment::CodePlanExecute {
                execution_phase,
                node_count,
                code_chars,
                run_ids,
                run_artifacts,
                ..
            } => {
                apply_code_plan_execute_counters(
                    &mut self.code_plans_executed,
                    &mut self.code_plan_code_chars,
                    &mut self.code_plan_nodes,
                    &mut self.code_plan_derived_runs,
                    execution_phase,
                    *node_count,
                    *code_chars,
                    run_ids.len(),
                    run_artifacts.len(),
                );
            }
            TraceSegment::PlasmError { .. } => {}
        }
    }

    /// Append one event and update top-level char / call counters (same semantics as the agent trace hub).
    /// Returns how many oldest timeline events were dropped to respect [`Self::timeline_max_events`].
    pub fn push_event(&mut self, ev: TraceEvent) -> u64 {
        self.apply_event_counters(&ev);
        self.records.push_back(ev);
        let mut dropped = 0u64;
        let cap = self.timeline_max_events.max(1);
        while self.records.len() > cap {
            self.records.pop_front();
            dropped = dropped.saturating_add(1);
        }
        dropped
    }
}

/// Rebuild [`SessionTraceData`] from a durable event list (idempotent fold).
pub fn session_data_from_events(
    mcp_session_id: impl Into<String>,
    events: &[TraceEvent],
) -> SessionTraceData {
    let mut data = SessionTraceData::new(mcp_session_id);
    for ev in events {
        let _ = data.push_event(ev.clone());
    }
    data
}

/// Rebuild [`SessionTraceData`] from owned events in timeline order (no per-event clone).
pub fn session_data_from_ordered_events(
    mcp_session_id: impl Into<String>,
    events: Vec<TraceEvent>,
) -> SessionTraceData {
    let mut data = SessionTraceData::new(mcp_session_id);
    for ev in events {
        let _ = data.push_event(ev);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TraceSegment;

    #[test]
    fn timeline_window_drops_oldest_but_retains_totals() {
        let mut d = SessionTraceData::new_with_timeline_cap("s1", 2);
        for i in 0..5u64 {
            let ev = TraceEvent::at(
                i,
                TraceSegment::PlasmLine {
                    call_index: 1,
                    line_index: i as usize,
                    source_expression: "x".into(),
                    repl_pre: String::new(),
                    repl_post: String::new(),
                    capability: None,
                    operation: "op".into(),
                    api_entry_id: None,
                    duration_ms: 10,
                    stats: plasm_runtime::ExecutionStats {
                        duration_ms: 10,
                        network_requests: 0,
                        cache_hits: 0,
                        cache_misses: 0,
                        cache: plasm_runtime::CacheTelemetry::default(),
                    },
                    source: plasm_runtime::ExecutionSource::Live,
                    request_fingerprints: vec![],
                    http_calls: vec![],
                },
            );
            let dropped = d.push_event(ev);
            if i < 2 {
                assert_eq!(dropped, 0, "i={i}");
            } else {
                assert_eq!(dropped, 1, "i={i}");
            }
        }
        assert_eq!(d.records.len(), 2);
        let totals = crate::totals_from_session_data(&d);
        assert_eq!(totals.expression_lines, 5);
        assert_eq!(totals.total_duration_ms, 50);
    }

    #[test]
    fn mcp_resource_read_duration_rolls_into_aggregate_total() {
        let mut d = SessionTraceData::new("s1");
        let ev = TraceEvent::at(
            1,
            TraceSegment::McpResourceRead {
                archive: None,
                uri_display: "plasm://run/1".into(),
                chars_added: 100,
                is_binary: false,
                duration_ms: 42,
                result: "success".into(),
                error_class: None,
                read_source: None,
            },
        );
        let _ = d.push_event(ev);
        let totals = crate::totals_from_session_data(&d);
        assert_eq!(totals.mcp_resource_read_chars, 100);
        assert_eq!(totals.total_duration_ms, 42);
    }

    #[test]
    fn code_plan_execute_failed_does_not_increment_executed_kpi() {
        use crate::CODE_PLAN_EXECUTION_FAILED;
        let mut d = SessionTraceData::new("s1");
        for phase in [
            CODE_PLAN_EXECUTION_FAILED,
            crate::CODE_PLAN_EXECUTION_STARTED,
        ] {
            let ev = TraceEvent::at(
                1,
                TraceSegment::CodePlanExecute {
                    plan_handle: "p1".into(),
                    plan_id: "id".into(),
                    plan_name: "demo".into(),
                    plan_hash: "abc".into(),
                    plan_uri: String::new(),
                    canonical_plan_uri: String::new(),
                    plan_http_path: String::new(),
                    prompt_hash: "p".repeat(64),
                    session_id: "s1".into(),
                    node_count: 2,
                    code_chars: 10,
                    comp: None,
                    dag: None,
                    plasm_call_index: Some(1),
                    run_ids: vec![],
                    run_artifacts: vec![],
                    plan_ux_reflection: None,
                    execution_phase: phase.to_string(),
                },
            );
            let _ = d.push_event(ev);
        }
        let totals = crate::totals_from_session_data(&d);
        assert_eq!(totals.code_plans_executed, 0);
    }
}
