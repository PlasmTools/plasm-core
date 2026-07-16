//! Canonical agent-facing MCP `content` presentation (compact TSV + optional prose body).
//!
//! All model-visible tokens for `plasm_context` / `plasm` / `plasm_run` live in tool **`content`**.
//! MCP Apps receive full payloads only via `structuredContent.ui` (negotiated full lane) or
//! app-only hydration tools — never via an agent `structuredContent.plasm` compromise lane.

use serde_json::{Map, Value};
use std::fmt::Write as _;

/// Stable fence info for the agent token table.
pub const AGENT_TOKEN_FENCE: &str = "tsv";

/// Keys emitted in the agent token table (order stable for snapshots).
#[allow(dead_code)] // contract checklist for token key set / ordering
pub const AGENT_TOKEN_KEYS: &[&str] = &[
    "kind",
    "run_ref",
    "dry_verdict",
    "dry_run",
    "logical_session_ref",
    "result_delivery",
    "artifact_uri",
];

/// Discriminator for agent-facing tool results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentResultKind {
    Context,
    Plan,
    Run,
}

impl AgentResultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Plan => "plan",
            Self::Run => "run",
        }
    }
}

/// Slim plan tokens for dry-run agent content.
#[derive(Debug, Clone)]
pub struct PlanTokenRefs<'a> {
    pub run_ref: &'a str,
    pub dry_verdict: &'a str,
}

/// Slim context tokens for `plasm_context` agent content.
#[derive(Debug, Clone)]
pub struct ContextTokenRefs<'a> {
    pub logical_session_ref: &'a str,
}

/// Slim run tokens with `artifact_uri` preferred over `run_id` (single canonical lookup).
#[derive(Debug, Clone, Default)]
pub struct RunTokens {
    pub result_delivery: Option<String>,
    pub artifact_uri: Option<String>,
    /// Only set when `artifact_uri` is absent.
    pub run_id: Option<String>,
}

impl RunTokens {
    /// Prefer a single `artifact_uri`; suppress `run_id` when `artifact_uri` is present.
    pub fn from_artifact(
        result_delivery: Option<&str>,
        artifact: Option<&crate::run_artifacts::RunArtifactHandle>,
    ) -> Self {
        let (artifact_uri, run_id) = match artifact {
            Some(h) => {
                let uri = if !h.canonical_plasm_uri.is_empty() {
                    h.canonical_plasm_uri.clone()
                } else if !h.plasm_uri.is_empty() {
                    h.plasm_uri.clone()
                } else {
                    String::new()
                };
                if uri.is_empty() {
                    (None, Some(h.run_id.to_wire()))
                } else {
                    (Some(uri), None)
                }
            }
            None => (None, None),
        };
        Self {
            result_delivery: result_delivery
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            artifact_uri,
            run_id,
        }
    }

    /// Prefer a single `artifact_uri`; suppress `run_id` when `artifact_uri` is present.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_first_step(
        result_delivery: Option<&str>,
        first_step: Option<&crate::mcp_plasm_meta::RunUiStepFields>,
    ) -> Self {
        Self::from_artifact(
            result_delivery,
            first_step.and_then(|s| s.artifact.as_ref()),
        )
    }

    /// Live MCP exit: typed artifact handle + slim fields from finalized `_meta.plasm`.
    pub fn from_live_result(
        plasm: &Map<String, Value>,
        first_artifact: Option<&crate::run_artifacts::RunArtifactHandle>,
    ) -> Self {
        let mut tokens = Self::from_artifact(
            plasm.get("result_delivery").and_then(|v| v.as_str()),
            first_artifact,
        );
        // If no typed artifact, fall back to plasm step JSON refs.
        if tokens.artifact_uri.is_none() && tokens.run_id.is_none() {
            let from_json = Self::from_plasm_obj(plasm);
            tokens.artifact_uri = from_json.artifact_uri;
            tokens.run_id = from_json.run_id;
        }
        tokens
    }

    /// Build from a `_meta.plasm` object (tests / last-resort JSON bridge).
    pub fn from_plasm_obj(obj: &Map<String, Value>) -> Self {
        let result_delivery = string_field(obj, "result_delivery");

        let top_artifact = string_field(obj, "artifact_uri");
        let step_artifact = first_step_string(obj, "artifact_uri")
            .or_else(|| first_step_string(obj, "canonical_artifact_uri"));
        let artifact_uri = top_artifact.or(step_artifact);

        let run_id = if artifact_uri.is_some() {
            None
        } else {
            string_field(obj, "run_id").or_else(|| first_step_string(obj, "run_id"))
        };

        Self {
            result_delivery,
            artifact_uri,
            run_id,
        }
    }
}

/// Canonical agent-facing tool `content`: discriminator + ordered tokens + optional body.
#[derive(Debug, Clone)]
pub struct AgentContent {
    kind: AgentResultKind,
    tokens: Vec<(&'static str, String)>,
    body: Option<String>,
    run_instruction: Option<String>,
}

impl AgentContent {
    pub fn plan(refs: &PlanTokenRefs<'_>, plan_body: &str) -> Self {
        let tokens: Vec<(&'static str, String)> = vec![
            ("kind", AgentResultKind::Plan.as_str().into()),
            ("run_ref", refs.run_ref.into()),
            ("dry_verdict", refs.dry_verdict.into()),
            ("dry_run", "true".into()),
        ];
        let run_instruction = format!(
            "**Run:** pass `run_ref`: `{}` to **`plasm_run`**. Do not echo the program.",
            refs.run_ref
        );
        Self {
            kind: AgentResultKind::Plan,
            tokens,
            body: Some(format!("```text\n{}\n```", plan_body.trim_end())),
            run_instruction: Some(run_instruction),
        }
    }

    pub fn context(refs: &ContextTokenRefs<'_>, body_markdown: &str) -> Self {
        let tokens: Vec<(&'static str, String)> = vec![
            ("kind", AgentResultKind::Context.as_str().into()),
            ("logical_session_ref", refs.logical_session_ref.into()),
        ];
        let body = body_markdown.trim();
        Self {
            kind: AgentResultKind::Context,
            tokens,
            body: (!body.is_empty()).then(|| body.to_string()),
            run_instruction: None,
        }
    }

    pub fn run(tokens: RunTokens, body_markdown: &str) -> Self {
        let mut pairs: Vec<(&'static str, String)> =
            vec![("kind", AgentResultKind::Run.as_str().into())];
        if let Some(s) = tokens
            .result_delivery
            .filter(|s| !s.is_empty() && s != "inline")
        {
            pairs.push(("result_delivery", s));
        }
        if let Some(s) = tokens.artifact_uri.filter(|s| !s.is_empty()) {
            pairs.push(("artifact_uri", s));
        } else if let Some(s) = tokens.run_id.filter(|s| !s.is_empty()) {
            pairs.push(("run_id", s));
        }
        let body = body_markdown.trim();
        Self {
            kind: AgentResultKind::Run,
            tokens: pairs,
            body: (!body.is_empty()).then(|| body.to_string()),
            run_instruction: None,
        }
    }

    /// Sole renderer of the agent token TSV fence + body + instruction.
    pub fn render(&self) -> String {
        let token_refs: Vec<(&str, &str)> =
            self.tokens.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let header = render_agent_token_tsv(&token_refs);
        let mut out = header;
        if let Some(body) = &self.body {
            out.push_str("\n\n");
            out.push_str(body);
        }
        if let Some(instr) = &self.run_instruction {
            out.push_str("\n\n");
            out.push_str(instr);
        }
        let _ = self.kind;
        out
    }
}

/// Build a compact key/value TSV fence for agent context.
pub(crate) fn render_agent_token_tsv(tokens: &[(&str, &str)]) -> String {
    let mut body = String::from("key\tvalue\n");
    for (key, value) in tokens {
        if value.is_empty() {
            continue;
        }
        let _ = writeln!(body, "{}\t{}", escape_tsv_cell(key), escape_tsv_cell(value));
    }
    format!("```{AGENT_TOKEN_FENCE}\n{}\n```", body.trim_end())
}

fn escape_tsv_cell(s: &str) -> String {
    if s.contains('\t') || s.contains('\n') || s.contains('\r') {
        s.replace('\t', " ").replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}

fn string_field(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn first_step_string(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get("steps")
        .and_then(|v| v.as_array())
        .and_then(|steps| steps.first())
        .and_then(|step| step.get(key))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_content_plan_render_stable() {
        let md = AgentContent::plan(
            &PlanTokenRefs {
                run_ref: "pc2",
                dry_verdict: "ok",
            },
            "plan ok · 1n 1r → r1\n\n01 r1           query Label{}",
        )
        .render();
        assert!(md.contains("```tsv\nkey\tvalue\n"));
        assert!(md.contains("kind\tplan\n"));
        assert!(md.contains("run_ref\tpc2\n"));
        assert!(!md.contains("logical_session_ref\t"));
        assert!(!md.contains("plan_uri\t"));
        assert!(!md.contains("domain_revision\t"));
        assert!(md.contains("```text\nplan ok"));
        assert!(md.contains("plasm_run"));
    }

    #[test]
    fn agent_content_context_render_stable() {
        let md = AgentContent::context(
            &ContextTokenRefs {
                logical_session_ref: "l_ref",
            },
            "## teaching\nok",
        )
        .render();
        assert!(md.contains("kind\tcontext\n"));
        assert!(md.contains("logical_session_ref\tl_ref\n"));
        assert!(!md.contains("session_mode\tnew\n"));
        assert!(!md.contains("domain_revision\t"));
        assert!(md.contains("## teaching\nok"));
    }

    #[test]
    fn run_tokens_from_first_step_prefer_canonical_uri() {
        use crate::run_artifacts::{
            artifact_http_path, plasm_run_resource_uri, plasm_short_resource_uri,
            RunArtifactHandle, RunArtifactId,
        };
        let run = RunArtifactId::from_bytes([3u8; 32]);
        let ph = "ab".repeat(32);
        let sid = "c".repeat(32);
        let handle = RunArtifactHandle {
            run_id: run,
            resource_index: 1,
            plasm_uri: plasm_short_resource_uri(1),
            canonical_plasm_uri: plasm_run_resource_uri(&ph, &sid, &run),
            http_path: artifact_http_path(&ph, &sid, &run),
            payload_len: 0,
            request_fingerprints: vec![],
        };
        let step = crate::mcp_plasm_meta::RunUiStepFields {
            run_step: 1,
            return_label: "items".into(),
            display: "q".into(),
            row_count: 1,
            node_id: None,
            preview_entities: None,
            artifact: Some(handle.clone()),
            lossy_summary_fields: crate::output::LossySummaryFieldNames::default(),
            column_schema: None,
        };
        let tokens = RunTokens::from_first_step(Some("inline"), Some(&step));
        assert_eq!(
            tokens.artifact_uri.as_deref(),
            Some(handle.canonical_plasm_uri.as_str())
        );
        assert!(tokens.run_id.is_none());
        let md = AgentContent::run(tokens, "body").render();
        assert!(md.contains("kind\trun\n"));
        assert!(md.contains(&handle.canonical_plasm_uri));
        assert!(!md.contains("run_id\t"));
        assert!(!md.contains("result_delivery\tinline"));
        assert!(!md.contains("logical_session_ref\t"));
        assert!(!md.contains("domain_revision\t"));
        assert!(AGENT_TOKEN_KEYS.contains(&"artifact_uri"));
    }

    #[test]
    fn run_tokens_from_live_result_prefer_typed_artifact() {
        use crate::run_artifacts::{
            artifact_http_path, plasm_run_resource_uri, plasm_short_resource_uri,
            RunArtifactHandle, RunArtifactId,
        };
        let run = RunArtifactId::from_bytes([4u8; 32]);
        let ph = "ef".repeat(32);
        let sid = "d".repeat(32);
        let handle = RunArtifactHandle {
            run_id: run,
            resource_index: 2,
            plasm_uri: plasm_short_resource_uri(2),
            canonical_plasm_uri: plasm_run_resource_uri(&ph, &sid, &run),
            http_path: artifact_http_path(&ph, &sid, &run),
            payload_len: 0,
            request_fingerprints: vec![],
        };
        let plasm = json!({
            "logical_session_ref": "l_ref",
            "result_delivery": "inline",
            "domain_revision": 3,
            "steps": [{ "run_id": "ignored", "artifact_uri": "plasm://short" }]
        });
        let tokens = RunTokens::from_live_result(plasm.as_object().expect("obj"), Some(&handle));
        assert_eq!(
            tokens.artifact_uri.as_deref(),
            Some(handle.canonical_plasm_uri.as_str())
        );
        assert!(tokens.run_id.is_none());
    }

    #[test]
    fn run_tokens_prefer_artifact_uri_over_run_id() {
        let plasm = json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "result_delivery": "inline",
            "steps": [{
                "artifact_uri": "plasm://execute/ph/s/run/prabc",
                "run_id": "prabc"
            }]
        });
        let obj = plasm.as_object().expect("obj");
        let out = AgentContent::run(RunTokens::from_plasm_obj(obj), "## rows\n```tsv\na\tb\n```")
            .render();
        assert!(out.contains("kind\trun\n"));
        assert!(out.contains("artifact_uri\tplasm://execute/ph/s/run/prabc\n"));
        assert!(!out.contains("run_id\t"));
    }
}
