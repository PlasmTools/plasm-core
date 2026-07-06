//! MCP App view payload (SEP-1865 `io.modelcontextprotocol/ui`, stable 2026-01-26).
//!
//! ## Triple-lane `CallToolResult`
//!
//! | Lane | Reader | Purpose |
//! |------|--------|---------|
//! | `content` | Agent / model | Compact markdown or gate text only |
//! | `structuredContent.ui` | MCP App View (iframe) | Render/fetch payload — **not** model context |
//! | `structuredContent.plasm` | Agent compromise | Slim tokens (`run_ref`, `dry_verdict`, …) — View must not require |
//! | `_meta.ui.resourceUri` | View mount | Static template pointer (`ui://plasm/plan-review`, …) |
//!
//! ## `structuredContent.ui` shapes
//!
//! **Plan (`plasm` dry-run)** — `kind: "plan_review"`:
//! - Inline when under [`INLINE_UI_BYTE_BUDGET`]: `comp`, `plan_ux_reflection`
//! - Fetch when large: `plan_http_path`, `plan_uri`, `canonical_plan_uri`
//!
//! **Run (`plasm_run`)** — `kind: "run_explorer"`:
//! - `result_delivery`: `"inline"` | `"snapshot_only"`
//! - `steps[]`: `run_step`, `return_label`, `display`, `row_count`, optional `preview_entities`,
//!   optional artifact refs (`artifact_path`, `run_id`, `artifact_uri`, …)
//!
//! App-only hydration when hosts strip lanes: `plasm_ui_read_plan`, `plasm_ui_read_run`
//! (`_meta.ui.visibility: ["app"]` — host-enforced; server still registers on `tools/list`).

use rust_mcp_sdk::schema::{CallToolResult, ToolOutputSchema};
use serde_json::{Map, Value};

/// Max serialized bytes for inline `comp` + `plan_ux_reflection` in `structuredContent.ui`.
pub const INLINE_UI_BYTE_BUDGET: usize = 64 * 1024;

/// Inline plan DAG passed directly to the UI lane (not agent `_meta.plasm`).
#[derive(Debug, Clone)]
pub struct UiInlinePlanPayload {
    pub comp: Value,
    pub plan_ux_reflection: Value,
}

/// Sole MCP tool-result exit: merge `_meta`, attach MCP App UI, mirror agent payload.
pub fn finalize_mcp_tool_result(
    res: CallToolResult,
    mut tool_meta: Map<String, Value>,
    agent_plan_text: Option<&str>,
    inline_plan_ui: Option<UiInlinePlanPayload>,
) -> CallToolResult {
    crate::mcp_app::attach_mcp_app_ui_on_tool_meta(&mut tool_meta);
    let res = res.with_meta(Some(tool_meta));
    let res = mirror_plasm_structured_content(res, inline_plan_ui.as_ref());
    inject_structured_agent_plan_text(res, agent_plan_text)
}

/// Inject compact dry-run plan text into agent `structuredContent.plasm` only.
pub fn inject_structured_agent_plan_text(
    mut res: CallToolResult,
    agent_plan_text: Option<&str>,
) -> CallToolResult {
    let Some(text) = agent_plan_text.filter(|t| !t.is_empty()) else {
        return res;
    };
    let Some(structured) = res.structured_content.as_mut() else {
        return res;
    };
    let Some(plasm) = structured.get_mut("plasm").and_then(|v| v.as_object_mut()) else {
        return res;
    };
    plasm.insert("plan_text".to_string(), Value::String(text.to_string()));
    res
}

/// Copy slim agent `_meta.plasm` into `structuredContent.plasm` and build `structuredContent.ui`.
pub fn mirror_plasm_structured_content(
    res: CallToolResult,
    inline_plan_ui: Option<&UiInlinePlanPayload>,
) -> CallToolResult {
    let Some(meta) = res.meta.as_ref() else {
        return res;
    };
    let Some(plasm) = meta.get("plasm") else {
        return res;
    };
    let structured_plasm = agent_structured_plasm_mirror(plasm);
    let mut structured = Map::new();
    structured.insert("plasm".to_string(), structured_plasm);
    if let Some(ui) = structured_ui_payload_from_meta(plasm, inline_plan_ui) {
        structured.insert("ui".to_string(), ui);
    }
    res.with_structured_content(structured)
}

const AGENT_STRUCTURED_DENY: &[&str] = &[
    "comp",
    "plan_ux_reflection",
    "program",
    "steps",
    "plan",
    "plan_http_path",
    "canonical_plan_uri",
    "index_delta",
];

const UI_STEP_REF_KEYS: &[&str] = &[
    "run_step",
    "return_label",
    "display",
    "row_count",
    "node_id",
    "artifact_uri",
    "canonical_artifact_uri",
    "artifact_path",
    "run_id",
];

const UI_STEP_INLINE_KEYS: &[&str] = &[
    "preview_entities",
    "column_schema",
    "lossy_summary_fields",
    "artifact_complete",
    "dict_ref",
];

const UI_PLAN_REF_KEYS: &[&str] = &["plan_uri", "plan_http_path", "canonical_plan_uri"];

const UI_PLAN_INLINE_KEYS: &[&str] = &["comp", "plan_ux_reflection"];

/// Whether inline plan DAG + UX reflection fit the UI byte budget.
pub fn inline_ui_payload_fits(comp: &Value, plan_ux: &Value) -> bool {
    let comp_len = serde_json::to_string(comp).map(|s| s.len()).unwrap_or(usize::MAX);
    let ux_len = serde_json::to_string(plan_ux).map(|s| s.len()).unwrap_or(usize::MAX);
    comp_len.saturating_add(ux_len) <= INLINE_UI_BYTE_BUDGET
}

fn ui_kind_from_plasm(obj: &Map<String, Value>) -> Option<&'static str> {
    if obj.get("dry_run").and_then(|v| v.as_bool()) == Some(true) {
        return Some("plan_review");
    }
    if obj
        .get("steps")
        .and_then(|v| v.as_array())
        .is_some_and(|steps| !steps.is_empty())
    {
        return Some("run_explorer");
    }
    if obj.get("continuity").and_then(|v| v.as_object()).is_some()
        && (obj.get("op").and_then(|v| v.as_object()).is_some()
            || obj.get("auto_async").and_then(|v| v.as_bool()) == Some(true))
    {
        return Some("run_explorer");
    }
    None
}

/// View lane payload from `_meta.plasm` (spec `structuredContent.ui`).
fn structured_ui_payload_from_meta(
    plasm: &Value,
    inline_plan_ui: Option<&UiInlinePlanPayload>,
) -> Option<Value> {
    let obj = plasm.as_object()?;
    let kind = ui_kind_from_plasm(obj)?;
    let mut out = Map::new();
    out.insert("kind".to_string(), Value::String(kind.to_string()));

    if kind == "plan_review" {
        for key in UI_PLAN_REF_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert(key.to_string(), v.clone());
            }
        }
        if let Some(inline) = inline_plan_ui {
            out.insert("comp".to_string(), inline.comp.clone());
            out.insert(
                "plan_ux_reflection".to_string(),
                inline.plan_ux_reflection.clone(),
            );
        } else {
            for key in UI_PLAN_INLINE_KEYS {
                if let Some(v) = obj.get(*key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
        }
    } else {
        if let Some(rd) = obj.get("result_delivery") {
            out.insert("result_delivery".to_string(), rd.clone());
        }
        if let Some(steps) = obj.get("steps").and_then(|v| v.as_array()) {
            let refs: Vec<Value> = steps
                .iter()
                .filter_map(|step| step_payload_from_step(step.as_object()?))
                .collect();
            if !refs.is_empty() {
                out.insert("steps".to_string(), Value::Array(refs));
            }
        }
    }

    Some(Value::Object(out))
}

fn step_payload_from_step(step: &Map<String, Value>) -> Option<Value> {
    let mut out = Map::new();
    for key in UI_STEP_REF_KEYS
        .iter()
        .chain(UI_STEP_INLINE_KEYS.iter())
    {
        if let Some(v) = step.get(*key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

fn agent_structured_plasm_mirror(plasm: &Value) -> Value {
    let Some(obj) = plasm.as_object() else {
        return plasm.clone();
    };
    let mut out = Map::new();
    for (key, value) in obj {
        if AGENT_STRUCTURED_DENY.contains(&key.as_str()) {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    Value::Object(out)
}

/// Extract inner `_meta.plasm` object from a partial tool meta map for finalization.
pub fn plasm_obj_from_tool_meta(meta: Map<String, Value>) -> Option<Map<String, Value>> {
    meta.get("plasm").and_then(|v| v.as_object().cloned())
}

/// Build MCP `outputSchema` documenting triple-lane tool results for `plasm`.
pub fn plasm_tool_output_schema() -> ToolOutputSchema {
    tool_ui_output_schema("plan_review")
}

/// Build MCP `outputSchema` documenting triple-lane tool results for `plasm_run`.
pub fn plasm_run_tool_output_schema() -> ToolOutputSchema {
    tool_ui_output_schema("run_explorer")
}

fn tool_ui_output_schema(kind: &str) -> ToolOutputSchema {
    use std::collections::BTreeMap;

    let mut ui_props = BTreeMap::new();
    ui_props.insert(
        "kind".into(),
        json_schema_string("MCP App view payload discriminator"),
    );
    if kind == "plan_review" {
        ui_props.insert(
            "comp".into(),
            json_schema_ref("Plasm comp wire DAG (inline when small)"),
        );
        ui_props.insert(
            "plan_ux_reflection".into(),
            json_schema_ref("Plan UX reflection (inline when small)"),
        );
        ui_props.insert(
            "plan_http_path".into(),
            json_schema_string("HTTP GET path for plan archive JSON when not inlined"),
        );
        ui_props.insert(
            "plan_uri".into(),
            json_schema_string("MCP resources/read URI for plan archive when not inlined"),
        );
    } else {
        ui_props.insert(
            "result_delivery".into(),
            json_schema_string("`inline` or `snapshot_only`"),
        );
        ui_props.insert(
            "steps".into(),
            json_schema_ref("Run step render/fetch descriptors with optional preview_entities"),
        );
    }

    let mut structured_props = BTreeMap::new();
    structured_props.insert(
        "ui".into(),
        json_schema_object("View render lane (SEP-1865 structuredContent.ui)", ui_props),
    );
    structured_props.insert(
        "plasm".into(),
        json_schema_ref("Agent compromise tokens only (run_ref, dry_verdict, …)"),
    );

    ToolOutputSchema::new(
        vec![],
        Some(structured_props),
        Some("https://json-schema.org/draft/2020-12/schema".into()),
    )
}

fn json_schema_string(description: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("string".into()));
    m.insert("description".into(), Value::String(description.into()));
    m
}

fn json_schema_ref(description: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("description".into(), Value::String(description.into()));
    m
}

fn json_schema_object(
    description: &str,
    properties: std::collections::BTreeMap<String, Map<String, Value>>,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("object".into()));
    m.insert("description".into(), Value::String(description.into()));
    m.insert(
        "properties".into(),
        Value::Object(
            properties
                .into_iter()
                .map(|(k, v)| (k, Value::Object(v)))
                .collect(),
        ),
    );
    m
}

/// Build app-only hydration tool result with plan DAG in `structuredContent.ui`.
pub fn ui_read_plan_tool_result(comp: Value, plan_ux_reflection: Value) -> CallToolResult {
    let mut ui = Map::new();
    ui.insert("kind".into(), Value::String("plan_review".into()));
    ui.insert("comp".into(), comp);
    ui.insert("plan_ux_reflection".into(), plan_ux_reflection);
    let mut structured = Map::new();
    structured.insert("ui".into(), Value::Object(ui));
    CallToolResult::text_content(vec![])
        .with_structured_content(structured)
}

/// Build app-only hydration tool result with run snapshot rows in `structuredContent.ui`.
pub fn ui_read_run_tool_result(
    run_id: &str,
    return_label: &str,
    row_count: usize,
    preview_entities: Value,
) -> CallToolResult {
    let step = serde_json::json!({
        "run_id": run_id,
        "return_label": return_label,
        "row_count": row_count,
        "preview_entities": preview_entities,
    });
    let mut ui = Map::new();
    ui.insert("kind".into(), Value::String("run_explorer".into()));
    ui.insert("result_delivery".into(), Value::String("inline".into()));
    ui.insert("steps".into(), Value::Array(vec![step]));
    let mut structured = Map::new();
    structured.insert("ui".into(), Value::Object(ui));
    CallToolResult::text_content(vec![]).with_structured_content(structured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{CallToolResult, TextContent};
    use serde_json::json;

    fn sample_dry_tool_meta() -> Map<String, Value> {
        let mut agent = Map::new();
        agent.insert("dry_run".into(), json!(true));
        agent.insert(
            "logical_session_ref".into(),
            json!("l_AAAAAAAAQACAAAAAAAAAAQ"),
        );
        agent.insert("run_ref".into(), json!("pc0"));
        agent.insert("dry_verdict".into(), json!("ok"));
        agent.insert(
            "plan_uri".into(),
            json!("plasm://execute/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/s1/plan/00000000-0000-4000-8000-000000000001"),
        );
        agent.insert(
            "plan_http_path".into(),
            json!("/execute/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/s1/plans/00000000-0000-4000-8000-000000000001"),
        );
        let mut meta = Map::new();
        meta.insert("plasm".into(), Value::Object(agent));
        meta
    }

    const SAMPLE_PLAN_TEXT: &str = "plan ok · 1n 1r → items\n\n01 items     query LangItem";

    #[test]
    fn finalize_dry_run_structured_content_is_slim_agent_tokens_only() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta(), Some(SAMPLE_PLAN_TEXT), None);
        let wire = serde_json::to_value(&out).expect("serialize CallToolResult");
        assert_eq!(
            wire.pointer("/structuredContent/plasm/run_ref")
                .and_then(|v| v.as_str()),
            Some("pc0")
        );
        assert!(
            wire.pointer("/structuredContent/plasm/plan_text")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("plan ok")),
            "agent structuredContent must carry compact plan_text only"
        );
        assert!(wire.pointer("/_meta/plasm/plan_text").is_none());
        assert!(wire.pointer("/structuredContent/plasm/comp").is_none());
        assert!(wire.pointer("/structuredContent/plasm/plan_http_path").is_none());
        assert!(wire.pointer("/structuredContent/plasm/plan_ux_reflection").is_none());
        assert_eq!(
            wire.pointer("/structuredContent/ui/kind")
                .and_then(|v| v.as_str()),
            Some("plan_review")
        );
        assert_eq!(
            wire.pointer("/structuredContent/ui/plan_uri")
                .and_then(|v| v.as_str()),
            Some("plasm://execute/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/s1/plan/00000000-0000-4000-8000-000000000001")
        );
        assert!(wire.pointer("/structuredContent/ui/comp").is_none());
    }

    #[test]
    fn finalize_inlines_small_plan_into_structured_content_ui() {
        let meta = sample_dry_tool_meta();
        let comp = json!({ "version": 1, "steps": { "n1": { "kind": "invoke" } }, "bind": { "topo": ["n1"] }, "return": { "kind": "step", "step": "n1" } });
        let ux = json!({ "schema_version": 3, "steps": [{ "operation": "query" }] });
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(
            res,
            meta,
            None,
            Some(UiInlinePlanPayload {
                comp: comp.clone(),
                plan_ux_reflection: ux.clone(),
            }),
        );
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/structuredContent/plasm/comp").is_none());
        assert!(wire.pointer("/_meta/plasm/comp").is_none());
        assert_eq!(wire.pointer("/structuredContent/ui/comp"), Some(&comp));
        assert_eq!(
            wire.pointer("/structuredContent/ui/plan_ux_reflection"),
            Some(&ux)
        );
    }

    #[test]
    fn finalize_attaches_plan_review_for_dry_run() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta(), None, None);
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_URI)
        );
    }

    #[test]
    fn finalize_attaches_run_explorer_for_live_steps() {
        let mut tool_meta = Map::new();
        tool_meta.insert(
            "plasm".into(),
            json!({
                "steps": [{ "run_step": 1, "return_label": "items", "row_count": 1 }]
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, tool_meta, None, None);
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_URI)
        );
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("kind"))
                .and_then(|v| v.as_str()),
            Some("run_explorer")
        );
    }

    #[test]
    fn mirror_live_run_strips_steps_from_structured_content_plasm() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
                "result_delivery": "inline",
                "steps": [{
                    "run_step": 1,
                    "return_label": "issue",
                    "row_count": 1,
                    "preview_entities": [{ "id": "1" }],
                    "artifact_uri": "plasm://session/l_AAAAAAAAQACAAAAAAAAAAQ/r/1",
                    "run_id": "prabc"
                }]
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)])
            .with_meta(Some(meta));
        let out = mirror_plasm_structured_content(res, None);
        assert!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("steps"))
                .is_none()
        );
        let ui_steps = out
            .structured_content
            .as_ref()
            .and_then(|m| m.get("ui"))
            .and_then(|u| u.get("steps"))
            .and_then(|s| s.as_array())
            .expect("structuredContent.ui.steps");
        assert_eq!(ui_steps.len(), 1);
        assert_eq!(
            ui_steps[0].get("preview_entities")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn mirror_live_run_strips_index_delta_from_structured_content() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
                "result_delivery": "inline",
                "meta_generation": 2,
                "index_delta": { "artifact_path": { "1": "/execute/ph/sid/artifacts/prabc" } },
                "steps": [{ "run_step": 1, "return_label": "repo", "row_count": 1 }]
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)])
            .with_meta(Some(meta));
        let out = mirror_plasm_structured_content(res, None);
        let plasm = out
            .structured_content
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .expect("structuredContent.plasm");
        assert!(plasm.get("index_delta").is_none());
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("result_delivery"))
                .and_then(|v| v.as_str()),
            Some("inline")
        );
    }

    #[test]
    fn inline_ui_payload_fits_respects_budget() {
        let small = json!({"a": 1});
        assert!(inline_ui_payload_fits(&small, &small));
        let huge = json!({"x": "y".repeat(INLINE_UI_BYTE_BUDGET)});
        assert!(!inline_ui_payload_fits(&huge, &small));
    }
}
