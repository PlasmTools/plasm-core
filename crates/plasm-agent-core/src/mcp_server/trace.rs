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
    pub store: Arc<crate::run_artifacts::RunArtifactStore>,
    pub mcp_key: &'a str,
    pub es: &'a ExecuteSession,
    pub prompt_hash: &'a str,
    pub session_id: &'a str,
    pub comp: Arc<TraceCompWire>,
    pub program: &'a str,
    pub plan_call_index: u64,
    pub code_chars: u64,
}

pub(crate) enum CodePlanTraceEmit<'a> {
    Evaluate {
        plan_ux_reflection: Option<serde_json::Value>,
        /// When true, skip durable plan archive write on the response critical path.
        defer_archive: bool,
    },
    Execute {
        phase: &'static str,
        /// `None` mints a fresh archive id (evaluate row, HTTP complete-only, or execute `started`).
        plan_id: Option<Uuid>,
        plan_ux_reflection: Option<serde_json::Value>,
        out: Option<&'a PlasmPlanRunResult>,
        archive_index_only: bool,
    },
}

/// HTTP/MCP refs emitted when archiving a code plan (evaluate or execute trace).
pub(crate) struct CodePlanEmitRefs {
    pub plan_uri: String,
    pub canonical_plan_uri: String,
    pub plan_http_path: String,
}

pub(crate) struct CodePlanTraceOutcome {
    pub refs: CodePlanEmitRefs,
    pub execute_plan_id: Option<StartedPlan>,
}

pub(crate) struct StartedPlan {
    pub id: Uuid,
    archived: bool,
    reflection: Option<serde_json::Value>,
}

/// Emit evaluate or execute code-plan trace segments (+ optional plan archive).
pub(crate) async fn emit_code_plan_trace(
    input: CodePlanTraceInput<'_>,
    emit: CodePlanTraceEmit<'_>,
) -> CodePlanTraceOutcome {
    let comp = input.comp.as_ref();
    // Preserve the exact submitted Python before execution can time out or be cancelled.
    // Failure keeps that document; completion may enrich it with final reflection.
    let skip_archive = matches!(
        &emit,
        CodePlanTraceEmit::Execute {
            phase: CODE_PLAN_EXECUTION_FAILED,
            ..
        }
    );
    let defer_archive = matches!(
        &emit,
        CodePlanTraceEmit::Evaluate {
            defer_archive: true,
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
    let archived_plan_ux = match &emit {
        CodePlanTraceEmit::Evaluate {
            plan_ux_reflection, ..
        }
        | CodePlanTraceEmit::Execute {
            plan_ux_reflection, ..
        } => plan_ux_reflection.clone(),
    };
    let initial_reflection = archived_plan_ux.clone();
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
        plan_ux_reflection: archived_plan_ux,
        catalog_cgs_hash: input.es.catalog_cgs_hash.clone(),
        domain_revision: input.es.domain_revision,
        entities: input.es.entities.clone(),
        principal: input.es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let canonical_plan_uri =
        plasm_code_plan_resource_uri(input.prompt_hash, input.session_id, &plan_id);
    let plan_http_path = code_plan_http_path(input.prompt_hash, input.session_id, &plan_id);
    let index_only = matches!(
        &emit,
        CodePlanTraceEmit::Execute {
            archive_index_only: true,
            ..
        }
    );
    let mut archived = false;
    if !skip_archive && !defer_archive {
        let result = if index_only {
            input
                .store
                .put_code_plan_index(input.prompt_hash, input.session_id, plan_index, plan_id)
                .await
        } else {
            input
                .store
                .insert_code_plan(
                    input.prompt_hash,
                    input.session_id,
                    plan_id,
                    plan_index,
                    &doc,
                )
                .await
                .map(|_| ())
        };
        if let Err(e) = result {
            tracing::warn!(
                target: "plasm_agent::mcp",
                error = %e,
                "failed to archive Plasm program plan for trace (non-fatal)"
            );
        } else {
            archived = true;
        }
    } else if defer_archive {
        crate::metrics::record_mcp_response_deferred_io("plan_archive");
        let store = Arc::clone(&input.store);
        let prompt_hash = input.prompt_hash.to_string();
        let session_id = input.session_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = store
                .insert_code_plan(
                    prompt_hash.as_str(),
                    session_id.as_str(),
                    plan_id,
                    plan_index,
                    &doc,
                )
                .await
            {
                tracing::warn!(
                    target: "plasm_agent::mcp",
                    error = %e,
                    "background plan archive write failed (non-fatal)"
                );
            }
        });
    }
    let refs = CodePlanEmitRefs {
        plan_uri: canonical_plan_uri.clone(),
        canonical_plan_uri,
        plan_http_path,
    };
    let node_count = plan_node_count_from_comp(comp);
    match emit {
        CodePlanTraceEmit::Evaluate {
            plan_ux_reflection,
            defer_archive: _,
        } => {
            input
                .hub
                .trace_record_code_plan_evaluate(
                    input.mcp_key,
                    crate::trace_hub::CodePlanEvaluateTrace {
                        plan_handle: handle_str,
                        plan_id: plan_id.to_string(),
                        plan_name: plan_display_name_from_comp(comp),
                        plan_hash: plan_hash_str,
                        plan_uri: refs.plan_uri.clone(),
                        canonical_plan_uri: refs.canonical_plan_uri.clone(),
                        plan_http_path: refs.plan_http_path.clone(),
                        prompt_hash: input.prompt_hash.to_string(),
                        session_id: input.session_id.to_string(),
                        node_count,
                        code_chars: input.code_chars,
                        comp: Arc::clone(&input.comp),
                        plan_ux_reflection,
                    },
                )
                .await;
            CodePlanTraceOutcome {
                refs,
                execute_plan_id: None,
            }
        }
        CodePlanTraceEmit::Execute {
            phase,
            plan_ux_reflection,
            out,
            ..
        } => {
            let (run_ids, run_artifacts) = out.map_or((Vec::new(), Vec::new()), |o| {
                let refs = run_artifact_refs_for_trace(o);
                (refs.iter().map(|a| a.run_id.clone()).collect(), refs)
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
                        plan_uri: refs.plan_uri.clone(),
                        canonical_plan_uri: refs.canonical_plan_uri.clone(),
                        plan_http_path: refs.plan_http_path.clone(),
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
            CodePlanTraceOutcome {
                refs,
                execute_plan_id: matches!(phase, CODE_PLAN_EXECUTION_STARTED).then_some(
                    StartedPlan {
                        id: plan_id,
                        archived,
                        reflection: initial_reflection,
                    },
                ),
            }
        }
    }
}

impl<'a> CodePlanTraceInput<'a> {
    pub(crate) async fn emit_evaluate(
        self,
        plan_ux_reflection: Option<serde_json::Value>,
        defer_archive: bool,
    ) -> CodePlanEmitRefs {
        emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Evaluate {
                plan_ux_reflection,
                defer_archive,
            },
        )
        .await
        .refs
    }

    pub(crate) async fn emit_execute_started(
        self,
        plan_ux_reflection: Option<serde_json::Value>,
    ) -> StartedPlan {
        emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Execute {
                phase: CODE_PLAN_EXECUTION_STARTED,
                plan_id: None,
                plan_ux_reflection,
                out: None,
                archive_index_only: false,
            },
        )
        .await
        .execute_plan_id
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
                archive_index_only: false,
            },
        )
        .await;
    }

    pub(crate) async fn emit_execute_completed(
        self,
        execute_plan_id: Option<StartedPlan>,
        plan_ux_reflection: Option<serde_json::Value>,
        out: &PlasmPlanRunResult,
    ) {
        let archive_index_only = execute_plan_id
            .as_ref()
            .is_some_and(|p| p.archived && p.reflection == plan_ux_reflection);
        emit_code_plan_trace(
            self,
            CodePlanTraceEmit::Execute {
                phase: CODE_PLAN_EXECUTION_COMPLETED,
                plan_id: execute_plan_id.as_ref().map(|p| p.id),
                plan_ux_reflection,
                out: Some(out),
                archive_index_only,
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

    #[tokio::test]
    async fn started_then_failed_preserves_original_python_archive() {
        use crate::run_artifacts::{CodePlanArchiveDocument, RunArtifactStore};
        use crate::test_support::session_fixtures::ExecuteSessionFixture;

        let hub = crate::trace_hub::TraceHub::default();
        let store = Arc::new(RunArtifactStore::default());
        let es = ExecuteSessionFixture::new()
            .entities(Vec::new())
            .build(Arc::new(plasm_core::CGS::default()));
        let source =
            "# café\nclass Probe(Program):\n    def build(self):\n        return e1.get()\n";
        let input = || super::CodePlanTraceInput {
            hub: &hub,
            store: Arc::clone(&store),
            mcp_key: "trace-source-test",
            es: &es,
            prompt_hash: "source-test",
            session_id: "session-test",
            comp: minimal_shared_comp(),
            program: source,
            plan_call_index: 1,
            code_chars: source.chars().count() as u64,
        };
        let plan_id = input().emit_execute_started(None).await.id;
        let before = store
            .get_code_plan_payload_result("source-test", "session-test", plan_id)
            .await
            .expect("archive read")
            .expect("source must be archived before execution begins");
        input().emit_execute_failed(plan_id).await;
        let after = store
            .get_code_plan_payload_result("source-test", "session-test", plan_id)
            .await
            .expect("archive read")
            .expect("failure must retain source");
        assert_eq!(before.bytes, after.bytes);
        let doc: CodePlanArchiveDocument =
            serde_json::from_slice(&after.bytes).expect("archive JSON");
        assert_eq!(doc.code, source);
        assert_eq!(doc.plan_id, plan_id.to_string());
    }

    #[tokio::test]
    async fn completed_plan_reuses_started_archive_and_repairs_index() {
        use crate::run_artifacts::{CodePlanArchiveDocument, RunArtifactStore};
        use crate::test_support::session_fixtures::ExecuteSessionFixture;

        let hub = crate::trace_hub::TraceHub::default();
        let store = Arc::new(RunArtifactStore::default());
        let es = ExecuteSessionFixture::new()
            .entities(Vec::new())
            .build(Arc::new(plasm_core::CGS::default()));
        let input = || super::CodePlanTraceInput {
            hub: &hub,
            store: Arc::clone(&store),
            mcp_key: "trace-test",
            es: &es,
            prompt_hash: "source-test",
            session_id: "session-test",
            comp: minimal_shared_comp(),
            program: "class P(Program): pass",
            plan_call_index: 1,
            code_chars: 22,
        };
        let reflection = serde_json::json!({"verdict":"allow"});
        let started = input().emit_execute_started(Some(reflection.clone())).await;
        let id = started.id;
        let before = store
            .get_code_plan_payload_result("source-test", "session-test", id)
            .await
            .unwrap()
            .unwrap();
        let doc: CodePlanArchiveDocument = serde_json::from_slice(&before.bytes).unwrap();
        assert_eq!(doc.plan_ux_reflection, Some(reflection.clone()));
        store
            .put_code_plan_index("source-test", "session-test", 1, uuid::Uuid::new_v4())
            .await
            .unwrap();
        let result = PlasmPlanRunResult {
            version: serde_json::Value::Null,
            agent_outcome: crate::plasm_plan_run::PlanAgentOutcome::Ready,
            node_results: vec![],
            graph_summary: serde_json::Value::Null,
            comp: None,
            code_plan_run_artifacts: vec![],
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: vec![],
            inline_plan_ui: None,
        };
        input()
            .emit_execute_completed(Some(started), Some(reflection), &result)
            .await;
        let after = store
            .get_code_plan_payload_result_by_index("source-test", "session-test", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            before.bytes, after.bytes,
            "completion updates the pointer without rewriting the document"
        );
        let missing_id = uuid::Uuid::new_v4();
        input()
            .emit_execute_completed(
                Some(super::StartedPlan {
                    id: missing_id,
                    archived: false,
                    reflection: None,
                }),
                Some(serde_json::json!({"verdict":"allow"})),
                &result,
            )
            .await;
        let recovered = store
            .get_code_plan_payload_result("source-test", "session-test", missing_id)
            .await
            .unwrap()
            .unwrap();
        let recovered: CodePlanArchiveDocument = serde_json::from_slice(&recovered.bytes).unwrap();
        assert_eq!(
            recovered.plan_id,
            missing_id.to_string(),
            "failed initial archive is written at completion"
        );
        let changed = input()
            .emit_execute_started(Some(serde_json::json!({"verdict":"review"})))
            .await;
        let changed_id = changed.id;
        let final_reflection = serde_json::json!({"verdict":"allow"});
        input()
            .emit_execute_completed(Some(changed), Some(final_reflection.clone()), &result)
            .await;
        let changed = store
            .get_code_plan_payload_result("source-test", "session-test", changed_id)
            .await
            .unwrap()
            .unwrap();
        let changed: CodePlanArchiveDocument = serde_json::from_slice(&changed.bytes).unwrap();
        assert_eq!(
            changed.plan_ux_reflection,
            Some(final_reflection),
            "changed final reflection is archived"
        );
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
            agent_outcome: Default::default(),
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
            inline_plan_ui: None,
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
