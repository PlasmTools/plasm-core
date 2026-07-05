//! MCP trace archive helpers for code-plan evaluate/execute.

use plasm_trace::{
    CodePlanRunArtifactRef, TraceCompWire, CODE_PLAN_EXECUTION_COMPLETED,
    CODE_PLAN_EXECUTION_FAILED, CODE_PLAN_EXECUTION_STARTED,
};

use crate::plasm_plan_run::PlasmPlanRunResult;

use super::*;

/// Full artifact refs for trace persistence (typed handles from orchestrator).
fn run_artifact_refs_for_trace(out: &PlasmPlanRunResult) -> Vec<CodePlanRunArtifactRef> {
    out.code_plan_run_artifacts.clone()
}

/// Shared inputs for evaluate / execute trace emission.
pub(crate) struct CodePlanTraceInput<'a> {
    pub hub: &'a crate::trace_hub::TraceHub,
    pub store: &'a crate::run_artifacts::RunArtifactStore,
    pub mcp_key: &'a str,
    pub es: &'a ExecuteSession,
    pub prompt_hash: &'a str,
    pub session_id: &'a str,
    pub session_ref: &'a str,
    pub comp: Arc<TraceCompWire>,
    pub program: &'a str,
    pub plan_call_index: u64,
    pub code_chars: u64,
}

pub(crate) enum CodePlanTraceEmit<'a> {
    Evaluate {
        plan_ux_reflection: Option<serde_json::Value>,
    },
    Execute {
        phase: &'static str,
        /// `None` mints a fresh archive id (evaluate row, HTTP complete-only, or execute `started`).
        plan_id: Option<Uuid>,
        plan_ux_reflection: Option<serde_json::Value>,
        out: Option<&'a PlasmPlanRunResult>,
    },
}

/// Emit evaluate or execute code-plan trace segments (+ optional plan archive).
pub(crate) async fn emit_code_plan_trace(
    input: CodePlanTraceInput<'_>,
    emit: CodePlanTraceEmit<'_>,
) -> Option<Uuid> {
    let comp = input.comp.as_ref();
    let skip_archive = matches!(
        &emit,
        CodePlanTraceEmit::Execute {
            phase: CODE_PLAN_EXECUTION_STARTED | CODE_PLAN_EXECUTION_FAILED,
            ..
        }
    );
    let plan_hash_str = comp_content_sha256_hex(comp);
    let plan_index = input.plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let plan_id = match &emit {
        CodePlanTraceEmit::Evaluate { .. } | CodePlanTraceEmit::Execute { plan_id: None, .. } => {
            Uuid::new_v4()
        }
        CodePlanTraceEmit::Execute {
            plan_id: Some(id), ..
        } => *id,
    };
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: input.prompt_hash.to_string(),
        session_id: input.session_id.to_string(),
        entry_id: input.es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(comp),
        code: input.program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: comp.to_json_value(),
        catalog_cgs_hash: input.es.catalog_cgs_hash.clone(),
        domain_revision: input.es.domain_revision,
        entities: input.es.entities.clone(),
        principal: input.es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = if skip_archive {
        (
            plasm_session_short_plan_uri(input.session_ref, plan_index),
            plasm_code_plan_resource_uri(input.prompt_hash, input.session_id, &plan_id),
            code_plan_http_path(input.prompt_hash, input.session_id, &plan_id),
        )
    } else {
        match input
            .store
            .insert_code_plan(
                input.prompt_hash,
                input.session_id,
                plan_id,
                plan_index,
                &doc,
            )
            .await
        {
            Ok(h) => (h.plasm_uri, h.canonical_plasm_uri, h.http_path),
            Err(e) => {
                tracing::warn!(
                    target: "plasm_agent::mcp",
                    error = %e,
                    "failed to archive Plasm program plan for trace (non-fatal)"
                );
                (
                    plasm_session_short_plan_uri(input.session_ref, plan_index),
                    plasm_code_plan_resource_uri(input.prompt_hash, input.session_id, &plan_id),
                    code_plan_http_path(input.prompt_hash, input.session_id, &plan_id),
                )
            }
        }
    };
    let node_count = plan_node_count_from_comp(comp);
    match emit {
        CodePlanTraceEmit::Evaluate { plan_ux_reflection } => {
            input
                .hub
                .trace_record_code_plan_evaluate(
                    input.mcp_key,
                    crate::trace_hub::CodePlanEvaluateTrace {
                        plan_handle: handle_str,
                        plan_id: plan_id.to_string(),
                        plan_name: plan_display_name_from_comp(comp),
                        plan_hash: plan_hash_str,
                        plan_uri,
                        canonical_plan_uri,
                        plan_http_path,
                        prompt_hash: input.prompt_hash.to_string(),
                        session_id: input.session_id.to_string(),
                        node_count,
                        code_chars: input.code_chars,
                        comp: Arc::clone(&input.comp),
                        plan_ux_reflection,
                    },
                )
                .await;
            None
        }
        CodePlanTraceEmit::Execute {
            phase,
            plan_ux_reflection,
            out,
            ..
        } => {
            let (run_ids, run_artifacts) = out.map_or((Vec::new(), Vec::new()), |o| {
                let refs = run_artifact_refs_for_trace(o);
                (
                    refs.iter().map(|a| a.run_id.clone()).collect(),
                    refs,
                )
            });
            input
                .hub
                .trace_record_code_plan_execute(
                    input.mcp_key,
                    crate::trace_hub::CodePlanExecuteTrace {
                        plan_handle: handle_str,
                        plan_id: plan_id.to_string(),
                        plan_name: plan_display_name_from_comp(comp),
                        plan_hash: plan_hash_str,
                        plan_uri,
                        canonical_plan_uri,
                        plan_http_path,
                        prompt_hash: input.prompt_hash.to_string(),
                        session_id: input.session_id.to_string(),
                        node_count,
                        code_chars: input.code_chars,
                        comp: Arc::clone(&input.comp),
                        plan_ux_reflection,
                        plasm_call_index: Some(input.plan_call_index),
                        run_ids,
                        run_artifacts,
                        execution_phase: phase.to_string(),
                    },
                )
                .await;
            Some(plan_id)
        }
    }
}

impl<'a> CodePlanTraceInput<'a> {
    pub(crate) async fn emit_evaluate(self, plan_ux_reflection: Option<serde_json::Value>) {
        emit_code_plan_trace(self, CodePlanTraceEmit::Evaluate { plan_ux_reflection }).await;
    }

    pub(crate) async fn emit_execute_started(self) -> Uuid {
        emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Execute {
                phase: CODE_PLAN_EXECUTION_STARTED,
                plan_id: None,
                plan_ux_reflection: None,
                out: None,
            },
        )
        .await
        .expect("execute started trace always returns plan_id")
    }

    pub(crate) async fn emit_execute_failed(self, execute_plan_id: Uuid) {
        let _ = emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Execute {
                phase: CODE_PLAN_EXECUTION_FAILED,
                plan_id: Some(execute_plan_id),
                plan_ux_reflection: None,
                out: None,
            },
        )
        .await;
    }

    pub(crate) async fn emit_execute_completed(
        self,
        execute_plan_id: Option<Uuid>,
        plan_ux_reflection: Option<serde_json::Value>,
        out: &PlasmPlanRunResult,
    ) {
        emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Execute {
                phase: CODE_PLAN_EXECUTION_COMPLETED,
                plan_id: execute_plan_id,
                plan_ux_reflection,
                out: Some(out),
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use plasm_trace::{
        minimal_trace_comp_json, CodePlanRunArtifactRef, SessionTraceData, TraceCompWire,
        TraceEvent, TraceSegment, CODE_PLAN_EXECUTION_FAILED, CODE_PLAN_EXECUTION_STARTED,
    };

    use crate::plasm_plan_run::PlasmPlanRunResult;

    use super::run_artifact_refs_for_trace;

    fn minimal_shared_comp() -> Arc<TraceCompWire> {
        Arc::new(TraceCompWire::from_json_value(minimal_trace_comp_json()).expect("minimal comp"))
    }

    fn minimal_execute_segment(phase: &str, plan_id: &str) -> TraceSegment {
        TraceSegment::CodePlanExecute {
            plan_handle: "p1".into(),
            plan_id: plan_id.into(),
            plan_name: "demo".into(),
            plan_hash: "abc".into(),
            plan_uri: String::new(),
            canonical_plan_uri: String::new(),
            plan_http_path: String::new(),
            prompt_hash: "p".repeat(64),
            session_id: "s1".into(),
            node_count: 2,
            code_chars: 10,
            comp: minimal_shared_comp(),
            plasm_call_index: Some(1),
            run_ids: vec![],
            run_artifacts: vec![],
            plan_ux_reflection: None,
            execution_phase: phase.into(),
        }
    }

    #[test]
    fn run_artifact_refs_for_trace_uses_typed_orchestrator_refs() {
        let out = PlasmPlanRunResult {
            version: serde_json::json!(1),
            node_results: Vec::new(),
            graph_summary: serde_json::json!({}),
            comp: None,
            code_plan_run_artifacts: vec![CodePlanRunArtifactRef {
                run_id: format!("pr{}", "ab".repeat(64)),
                artifact_uri: Some("plasm://session/l_x/run/prab".into()),
                run_step: Some(2),
                ..Default::default()
            }],
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: Vec::new(),
        };
        let refs = run_artifact_refs_for_trace(&out);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].run_step, Some(2));
        assert_eq!(
            refs[0].artifact_uri.as_deref(),
            Some("plasm://session/l_x/run/prab")
        );
    }

    #[test]
    fn execute_failed_phase_does_not_count_as_executed_kpi() {
        let mut d = SessionTraceData::new("s1");
        let _ = d.push_event(TraceEvent::at(
            1,
            minimal_execute_segment(CODE_PLAN_EXECUTION_FAILED, "pid-failed"),
        ));
        assert_eq!(d.code_plans_executed, 0);
    }

    #[test]
    fn execute_started_with_comp_has_bind_topology() {
        let mut comp_json = minimal_trace_comp_json();
        comp_json["steps"]["a"] = serde_json::json!({
            "kind": "invoke",
            "plan_kind": "query",
            "effect_class": "read",
            "result_shape": "list"
        });
        comp_json["bind"]["topo"] = serde_json::json!(["a"]);
        comp_json["return"]["step"] = serde_json::json!("a");
        let comp = Arc::new(TraceCompWire::from_json_value(comp_json).expect("valid comp"));
        let mut d = SessionTraceData::new("s1");
        let seg = TraceSegment::CodePlanExecute {
            plan_handle: "p1".into(),
            plan_id: "pid-started".into(),
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
            plasm_call_index: Some(1),
            run_ids: vec![],
            run_artifacts: vec![],
            plan_ux_reflection: None,
            execution_phase: CODE_PLAN_EXECUTION_STARTED.into(),
        };
        let _ = d.push_event(TraceEvent::at(1, seg));
        assert_eq!(d.code_plans_executed, 0);
    }
}
