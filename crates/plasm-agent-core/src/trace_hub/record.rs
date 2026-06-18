//! Public trace record helpers (domain segments, plasm lines, resource reads).

use plasm_runtime::http_trace::HttpTraceEntry;
use plasm_runtime::ExecutionResult;
use plasm_trace::{PlasmLineTraceMeta, RunArtifactArchiveRef, TraceEvent, TraceSegment};

use super::state::TraceIngestJob;
use super::{
    now_ms, truncate_trace_reasoning, CodePlanEvaluateTrace, CodePlanExecuteTrace,
    PlasmContextTrace, TraceHub, TraceSsePayload,
};

impl TraceHub {
    pub async fn trace_note_teaching_prompt_chars(&self, mcp_key: &str, chars_added: u64) {
        if chars_added == 0 {
            return;
        }
        self.bump_and_emit(
            mcp_key,
            TraceSegment::TeachingPromptCharsDelta { chars_added },
        )
        .await;
    }

    pub async fn trace_record_plasm_context(&self, mcp_key: &str, trace: PlasmContextTrace) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmContext {
                teaching_prompt_chars_added: trace.teaching_prompt_chars_added,
                reused_session: trace.reused_session,
                mode: trace.mode,
                entry_id: trace.entry_id,
                entities: trace.entities,
                seeds: trace.seeds,
            },
        )
        .await;
    }

    pub async fn trace_record_expand_domain(
        &self,
        mcp_key: &str,
        teaching_prompt_chars_added: u64,
        entry_id: Option<String>,
        entities: Vec<String>,
        seeds: Vec<String>,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::ExpandDomain {
                teaching_prompt_chars_added,
                entry_id,
                entities,
                seeds,
            },
        )
        .await;
    }

    pub async fn trace_record_code_plan_evaluate(
        &self,
        mcp_key: &str,
        trace: CodePlanEvaluateTrace,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::CodePlanEvaluate {
                plan_handle: trace.plan_handle,
                plan_id: trace.plan_id,
                plan_name: trace.plan_name,
                plan_hash: trace.plan_hash,
                plan_uri: trace.plan_uri,
                canonical_plan_uri: trace.canonical_plan_uri,
                plan_http_path: trace.plan_http_path,
                prompt_hash: trace.prompt_hash,
                session_id: trace.session_id,
                node_count: trace.node_count,
                code_chars: trace.code_chars,
                comp: trace.comp,
                dag: trace.dag,
                plan_ux_reflection: trace.plan_ux_reflection,
            },
        )
        .await;
    }

    pub async fn trace_record_code_plan_execute(&self, mcp_key: &str, trace: CodePlanExecuteTrace) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::CodePlanExecute {
                plan_handle: trace.plan_handle,
                plan_id: trace.plan_id,
                plan_name: trace.plan_name,
                plan_hash: trace.plan_hash,
                plan_uri: trace.plan_uri,
                canonical_plan_uri: trace.canonical_plan_uri,
                plan_http_path: trace.plan_http_path,
                prompt_hash: trace.prompt_hash,
                session_id: trace.session_id,
                node_count: trace.node_count,
                code_chars: trace.code_chars,
                comp: trace.comp.clone(),
                dag: trace.dag.clone(),
                plasm_call_index: trace.plasm_call_index,
                run_ids: trace.run_ids,
                run_artifacts: trace.run_artifacts,
                plan_ux_reflection: trace.plan_ux_reflection,
                execution_phase: trace.execution_phase,
            },
        )
        .await;
    }

    /// Start of a `plasm` tool invocation. Returns monotonic `call_index` for line records.
    ///
    /// Intentionally not routed through [`Self::bump_and_emit`]: `call_index` allocation, event push,
    /// and `seq` ordering must stay aligned with subsequent [`Self::trace_add_plasm_line`] / error rows.
    pub async fn trace_record_plasm_invocation(
        &self,
        mcp_key: &str,
        multi_line: bool,
        expression_count: usize,
        reasoning_chars: Option<u64>,
        plasm_invocation_chars_added: u64,
        reasoning: Option<String>,
    ) -> u64 {
        if !self.ensure_active_for_emit(mcp_key).await {
            tracing::warn!(
                target: "plasm_agent::trace_hub",
                mcp_key,
                "plasm_invocation trace dropped: no active or resumable completed trace"
            );
            crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
            return 0;
        }
        let reasoning_stored = reasoning
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(truncate_trace_reasoning);
        let (call_index, trace_id, seq, record, job_opt) = {
            let mut g = self.inner.write().await;
            let Some(a) = g.active.get_mut(mcp_key) else {
                crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
                return 0;
            };
            let next_call = a.data.plasm_call_count.saturating_add(1);
            let ev = TraceEvent::at(
                now_ms(),
                TraceSegment::PlasmInvocation {
                    call_index: next_call,
                    multi_line,
                    expression_count,
                    plasm_invocation_chars_added,
                    reasoning_chars,
                    reasoning: reasoning_stored.clone(),
                },
            );
            let dropped = a.data.push_event(ev);
            if dropped > 0 {
                crate::metrics::record_trace_timeline_events_dropped(dropped);
            }
            let ev_ref = a
                .data
                .records
                .back()
                .expect("push_event always appends one record");
            let record = serde_json::to_value(ev_ref).unwrap_or_default();
            let job_opt = self.ingest_tx.as_ref().map(|_| {
                TraceIngestJob::new_mcp_active_segment(a, ev_ref.clone(), Some(record.clone()))
            });
            a.last_activity_ms = now_ms();
            a.seq = a.seq.saturating_add(1);
            let seq = a.seq;
            let trace_id = a.trace_id;
            (next_call, trace_id, seq, record, job_opt)
        };
        self.emit_json(trace_id, &TraceSsePayload::Patch { seq, record })
            .await;
        if let (Some(tx), Some(job)) = (self.ingest_tx.as_ref(), job_opt) {
            self.enqueue_durable_job_after_patch(tx, job, seq).await;
        }
        call_index
    }

    pub async fn trace_note_plasm_response_chars(
        &self,
        mcp_key: &str,
        chars: u64,
        tool: &str,
        call_index: u64,
        multi_line: bool,
        expression_count: usize,
    ) {
        if chars == 0 {
            return;
        }
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmResponseCharsDelta {
                chars_added: chars,
                tool: tool.to_string(),
                call_index: Some(call_index),
                multi_line,
                expression_count: Some(expression_count),
            },
        )
        .await;
    }

    /// MCP `resources/read` timeline row (payload size + archive ref for future web deep-links).
    #[allow(clippy::too_many_arguments)]
    pub async fn trace_record_mcp_resource_read(
        &self,
        mcp_key: &str,
        archive: Option<RunArtifactArchiveRef>,
        uri_display: String,
        chars_added: u64,
        is_binary: bool,
        duration_ms: u64,
        result: &str,
        error_class: Option<&str>,
        read_source: Option<&str>,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::McpResourceRead {
                archive,
                uri_display,
                chars_added,
                is_binary,
                duration_ms,
                result: result.to_string(),
                error_class: error_class.map(str::to_string),
                read_source: read_source.map(str::to_string),
            },
        )
        .await;
    }

    pub async fn trace_add_plasm_line(
        &self,
        mcp_key: &str,
        call_index: u64,
        line_index: usize,
        meta: PlasmLineTraceMeta,
        result: &ExecutionResult,
        http_calls: Vec<HttpTraceEntry>,
    ) {
        let rec = TraceSegment::PlasmLine {
            call_index,
            line_index,
            source_expression: meta.source_expression,
            repl_pre: meta.repl_pre,
            repl_post: meta.repl_post,
            capability: meta.capability,
            operation: meta.operation,
            api_entry_id: meta.api_entry_id,
            duration_ms: result.stats.duration_ms,
            stats: result.stats.clone(),
            source: result.source,
            request_fingerprints: result.request_fingerprints.clone(),
            http_calls,
        };
        self.bump_and_emit(mcp_key, rec).await;
    }

    pub async fn trace_add_plasm_error(
        &self,
        mcp_key: &str,
        call_index: u64,
        line_index: Option<usize>,
        message: String,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmError {
                call_index,
                line_index,
                message,
            },
        )
        .await;
    }
}
