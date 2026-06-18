//! One append-only trace segment (tool / domain / expression row).

use plasm_observability_contracts::RunArtifactArchiveRef;
use plasm_runtime::http_trace::HttpTraceEntry;
use plasm_runtime::{ExecutionSource, ExecutionStats};
use serde::{Deserialize, Serialize};

/// Source + REPL display strings recorded with each executed line trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlasmLineTraceMeta {
    pub source_expression: String,
    pub repl_pre: String,
    pub repl_post: String,
    pub capability: Option<String>,
    pub operation: String,
    pub api_entry_id: Option<String>,
}

/// Structured reference to a run snapshot produced while executing an archived Plasm program plan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodePlanRunArtifactRef {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_artifact_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_step: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_fingerprints: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Append-only trace segments (JSON-serializable for HTTP + SSE + Iceberg `payload_json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceSegment {
    PlasmContext {
        teaching_prompt_chars_added: u64,
        reused_session: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entities: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        seeds: Vec<String>,
    },
    ExpandDomain {
        teaching_prompt_chars_added: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entities: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        seeds: Vec<String>,
    },
    PlasmInvocation {
        call_index: u64,
        multi_line: bool,
        expression_count: usize,
        /// Character weight of this invocation (for aggregate replay from durable rows).
        plasm_invocation_chars_added: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_chars: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    PlasmLine {
        call_index: u64,
        line_index: usize,
        source_expression: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        repl_pre: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        repl_post: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_entry_id: Option<String>,
        duration_ms: u64,
        stats: ExecutionStats,
        source: ExecutionSource,
        request_fingerprints: Vec<String>,
        http_calls: Vec<HttpTraceEntry>,
    },
    PlasmError {
        call_index: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_index: Option<usize>,
        message: String,
    },
    /// Domain prompt character weight without a `plasm_context` / `expand_domain` row (rare; durable parity).
    TeachingPromptCharsDelta { chars_added: u64 },
    /// Response markdown character weight (MCP tool body sizing; pairs with successful `plasm` tool).
    PlasmResponseCharsDelta {
        chars_added: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_index: Option<u64>,
        #[serde(default, skip_serializing_if = "is_false")]
        multi_line: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expression_count: Option<usize>,
    },
    /// MCP `resources/read` on a run snapshot URI (size + timing + archive ref for future UI).
    McpResourceRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive: Option<RunArtifactArchiveRef>,
        /// Truncated request URI for display.
        uri_display: String,
        chars_added: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        is_binary: bool,
        duration_ms: u64,
        /// `"success"` or `"error"`.
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<String>,
    },
    CodePlanEvaluate {
        plan_handle: String,
        plan_id: String,
        plan_name: String,
        plan_hash: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        plan_uri: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        canonical_plan_uri: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        plan_http_path: String,
        prompt_hash: String,
        session_id: String,
        node_count: usize,
        code_chars: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comp: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dag: Option<serde_json::Value>,
    },
    CodePlanExecute {
        plan_handle: String,
        plan_id: String,
        plan_name: String,
        plan_hash: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        plan_uri: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        canonical_plan_uri: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        plan_http_path: String,
        prompt_hash: String,
        session_id: String,
        #[serde(default)]
        node_count: usize,
        #[serde(default)]
        code_chars: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comp: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dag: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plasm_call_index: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        run_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        run_artifacts: Vec<CodePlanRunArtifactRef>,
    },
}

#[cfg(test)]
mod tests {
    use super::TraceSegment;

    #[test]
    fn code_plan_trace_segments_carry_provenance() {
        let eval = TraceSegment::CodePlanEvaluate {
            plan_handle: "p1".into(),
            plan_id: "00000000-0000-0000-0000-000000000000".into(),
            plan_name: "demo".into(),
            plan_hash: "abc".into(),
            plan_uri: "plasm://session/s0/p/1".into(),
            canonical_plan_uri:
                "plasm://execute/pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp/s1/plan/00000000-0000-0000-0000-000000000000".into(),
            plan_http_path:
                "/execute/pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp/s1/plans/00000000-0000-0000-0000-000000000000".into(),
            prompt_hash: "p".repeat(64),
            session_id: "s1".into(),
            node_count: 2,
            code_chars: 42,
            comp: Some(serde_json::json!({"steps": {"n1": {}}, "bind": {"topo": ["n1"]}})),
            dag: None,
        };
        let v = serde_json::to_value(eval).expect("json");
        assert_eq!(v["kind"], "code_plan_evaluate");
        assert_eq!(v["plan_handle"], "p1");
        assert_eq!(v["comp"]["bind"]["topo"][0], "n1");

        let exec = TraceSegment::CodePlanExecute {
            plan_handle: "p1".into(),
            plan_id: "00000000-0000-0000-0000-000000000000".into(),
            plan_name: "demo".into(),
            plan_hash: "abc".into(),
            plan_uri: "plasm://session/s0/p/1".into(),
            canonical_plan_uri:
                "plasm://execute/pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp/s1/plan/00000000-0000-0000-0000-000000000000".into(),
            plan_http_path:
                "/execute/pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp/s1/plans/00000000-0000-0000-0000-000000000000".into(),
            prompt_hash: "p".repeat(64),
            session_id: "s1".into(),
            node_count: 2,
            code_chars: 42,
            comp: Some(serde_json::json!({"steps": {"n1": {}}, "bind": {"topo": ["n1"]}})),
            dag: None,
            plasm_call_index: Some(7),
            run_ids: vec!["r1".into()],
            run_artifacts: vec![super::CodePlanRunArtifactRef {
                run_id: "r1".into(),
                artifact_uri: Some("plasm://session/s0/r/1".into()),
                canonical_artifact_uri: Some("plasm://execute/p/s1/run/r1".into()),
                artifact_path: Some("/execute/p/s1/artifacts/r1".into()),
                run_step: Some(1),
                node_id: Some("n1".into()),
                display: Some("query".into()),
                request_fingerprints: vec!["fp".into()],
            }],
        };
        let v = serde_json::to_value(exec).expect("json");
        assert_eq!(v["kind"], "code_plan_execute");
        assert_eq!(v["plasm_call_index"], 7);
        assert_eq!(v["run_ids"][0], "r1");
        assert_eq!(v["run_artifacts"][0]["run_id"], "r1");
    }

    #[test]
    fn code_plan_trace_segments_accept_legacy_rows() {
        let legacy = serde_json::json!({
            "kind": "code_plan_execute",
            "plan_handle": "p1",
            "plan_id": "00000000-0000-0000-0000-000000000000",
            "plan_name": "demo",
            "plan_hash": "abc",
            "prompt_hash": "p".repeat(64),
            "session_id": "s1",
            "run_ids": ["r1"]
        });
        let seg: TraceSegment = serde_json::from_value(legacy).expect("legacy code plan trace");
        match seg {
            TraceSegment::CodePlanExecute {
                plan_uri,
                run_artifacts,
                node_count,
                ..
            } => {
                assert!(plan_uri.is_empty());
                assert!(run_artifacts.is_empty());
                assert_eq!(node_count, 0);
            }
            other => panic!("unexpected segment: {other:?}"),
        }
    }
}
