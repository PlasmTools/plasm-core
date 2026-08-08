//! MCP App view payload (SEP-1865 `io.modelcontextprotocol/ui`, stable 2026-01-26).
//!
//! ## Dual-lane `CallToolResult`
//!
//! | Lane | Reader | Purpose |
//! |------|--------|---------|
//! | `content` | Agent / model | Canonical compact TSV (+ plan body / row fences) |
//! | `structuredContent.ui` | MCP App View (iframe) | Full render/fetch payload — **not** model context |
//! | `_meta.ui.resourceUri` | View mount | Static template pointer (`ui://plasm/plan-review`, …) |
//! | `_meta.plasm` | Host / View / transport | Continuity, steps, artifact refs (hosts may strip) |
//!
//! There is **no** agent `structuredContent.plasm` lane. Tool-only hosts
//! ([`crate::mcp_delivery::McpDeliveryProfile::ToolFallback`]) receive **content only**
//! (plus optional `_meta` for mount pointers) so connectors that surface `structuredContent`
//! cannot suppress row data.
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

use crate::mcp_delivery::McpDeliveryProfile;

/// Max serialized bytes for inline `comp` + `plan_ux_reflection` in `structuredContent.ui`.
pub const INLINE_UI_BYTE_BUDGET: usize = 64 * 1024;

/// Inline plan DAG passed directly to the UI lane (not agent `_meta.plasm`).
#[derive(Debug, Clone)]
pub struct UiInlinePlanPayload {
    pub comp: Value,
    pub plan_ux_reflection: Value,
}

/// MCP App view payload discriminator (`structuredContent.ui.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPayloadKind {
    PlanReview,
    RunExplorer,
}

impl UiPayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlanReview => "plan_review",
            Self::RunExplorer => "run_explorer",
        }
    }

    /// Prefer typed flags from builders; JSON inference only for finalize projection.
    pub fn from_plasm_obj(obj: &Map<String, Value>) -> Option<Self> {
        if obj.get("dry_run").and_then(|v| v.as_bool()) == Some(true) {
            return Some(Self::PlanReview);
        }
        if obj
            .get("steps")
            .and_then(|v| v.as_array())
            .is_some_and(|steps| !steps.is_empty())
        {
            return Some(Self::RunExplorer);
        }
        if obj.get("continuity").and_then(|v| v.as_object()).is_some()
            && (obj.get("op").and_then(|v| v.as_object()).is_some()
                || obj.get("auto_async").and_then(|v| v.as_bool()) == Some(true))
        {
            return Some(Self::RunExplorer);
        }
        None
    }
}

/// Sole MCP tool-result exit: merge `_meta`, attach MCP App UI, optionally `structuredContent.ui`.
///
/// Callers must already put agent token TSV into `content` via
/// [`crate::mcp_agent_present::AgentContent`]. This exit never rebuilds agent TSV from `_meta.plasm`.
pub fn finalize_mcp_tool_result(
    res: CallToolResult,
    mut tool_meta: Map<String, Value>,
    profile: McpDeliveryProfile,
    inline_plan_ui: Option<UiInlinePlanPayload>,
) -> CallToolResult {
    if profile.attaches_ui_meta() {
        crate::mcp_app::attach_mcp_app_ui_on_tool_meta(&mut tool_meta);
    }
    let res = res.with_meta(Some(tool_meta));
    if profile.emits_structured_ui() {
        attach_structured_ui_lane(res, inline_plan_ui.as_ref())
    } else {
        // Tool-only / Apps-disabled: never emit structuredContent (Claude prefers it over text).
        clear_structured_content(res)
    }
}

/// Bundled dual-lane MCP tool result: rendered agent content + plasm meta + delivery profile.
#[derive(Debug, Clone)]
pub struct DualLaneToolResult {
    pub content: String,
    pub plasm_meta: Map<String, Value>,
    pub profile: McpDeliveryProfile,
    pub inline_plan_ui: Option<UiInlinePlanPayload>,
}

impl DualLaneToolResult {
    pub fn into_call_tool_result(self) -> CallToolResult {
        use rust_mcp_sdk::schema::TextContent;
        let res = CallToolResult::text_content(vec![TextContent::new(self.content, None, None)]);
        let mut meta = Map::new();
        meta.insert("plasm".into(), Value::Object(self.plasm_meta));
        finalize_mcp_tool_result(res, meta, self.profile, self.inline_plan_ui)
    }

    /// Build from a full tool `_meta` map that already nests `plasm`.
    pub fn from_tool_meta(
        content: String,
        mut tool_meta: Map<String, Value>,
        profile: McpDeliveryProfile,
        inline_plan_ui: Option<UiInlinePlanPayload>,
    ) -> Self {
        let plasm_meta = tool_meta
            .remove("plasm")
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .unwrap_or_default();
        Self {
            content,
            plasm_meta,
            profile,
            inline_plan_ui,
        }
    }
}

fn clear_structured_content(mut res: CallToolResult) -> CallToolResult {
    res.structured_content = None;
    res
}

/// Attach View lane `structuredContent.ui` only (no agent `plasm` namespace).
pub(crate) fn attach_structured_ui_lane(
    res: CallToolResult,
    inline_plan_ui: Option<&UiInlinePlanPayload>,
) -> CallToolResult {
    let Some(meta) = res.meta.as_ref() else {
        return clear_structured_content(res);
    };
    let Some(plasm) = meta.get("plasm") else {
        return clear_structured_content(res);
    };
    let Some(ui) = structured_ui_payload_from_meta(plasm, inline_plan_ui) else {
        return clear_structured_content(res);
    };
    let mut structured = Map::new();
    structured.insert("ui".to_string(), ui);
    res.with_structured_content(structured)
}

const UI_STEP_REF_KEYS: &[&str] = &[
    "run_step",
    "return_label",
    "display",
    "row_count",
    "node_id",
    "artifact_uri",
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
    let comp_len = serde_json::to_string(comp)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    let ux_len = serde_json::to_string(plan_ux)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    comp_len.saturating_add(ux_len) <= INLINE_UI_BYTE_BUDGET
}

/// View lane payload from `_meta.plasm` (spec `structuredContent.ui`).
///
/// Plan Review DAG (`comp` / `plan_ux_reflection`) is only attached for dry-run
/// [`UiPayloadKind::PlanReview`]. Fused clean-reads are run-shaped — Run Explorer only;
/// callers must not pass [`UiInlinePlanPayload`] on that path.
fn structured_ui_payload_from_meta(
    plasm: &Value,
    inline_plan_ui: Option<&UiInlinePlanPayload>,
) -> Option<Value> {
    let obj = plasm.as_object()?;
    let kind = UiPayloadKind::from_plasm_obj(obj)?;
    let mut out = Map::new();
    out.insert("kind".to_string(), Value::String(kind.as_str().to_string()));

    // Plan fetch refs only on Plan Review (dry-run / `run_ref` responses).
    if kind == UiPayloadKind::PlanReview {
        for key in UI_PLAN_REF_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }

    // Inline plan DAG: dry-run review only. Ignore accidental inline on run-shaped meta.
    if kind == UiPayloadKind::PlanReview {
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
    }

    if kind == UiPayloadKind::RunExplorer {
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
    for key in UI_STEP_REF_KEYS.iter().chain(UI_STEP_INLINE_KEYS.iter()) {
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

/// Borrow inner `_meta.plasm` object from a partial tool meta map.
#[cfg(test)]
pub(crate) fn take_plasm_map(meta: &Map<String, Value>) -> Option<&Map<String, Value>> {
    meta.get("plasm").and_then(|v| v.as_object())
}

/// Build MCP `outputSchema` documenting dual-lane tool results for `plasm`.
pub fn plasm_tool_output_schema() -> ToolOutputSchema {
    tool_ui_output_schema(UiPayloadKind::PlanReview)
}

/// Build MCP `outputSchema` documenting dual-lane tool results for `plasm_run`.
pub fn plasm_run_tool_output_schema() -> ToolOutputSchema {
    tool_ui_output_schema(UiPayloadKind::RunExplorer)
}

fn tool_ui_output_schema(kind: UiPayloadKind) -> ToolOutputSchema {
    use std::collections::BTreeMap;

    let mut ui_props = BTreeMap::new();
    ui_props.insert(
        "kind".into(),
        json_schema_string("MCP App view payload discriminator"),
    );
    if kind == UiPayloadKind::PlanReview {
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

fn call_tool_result_with_ui(ui: Map<String, Value>) -> CallToolResult {
    let mut structured = Map::new();
    structured.insert("ui".into(), Value::Object(ui));
    CallToolResult::text_content(vec![]).with_structured_content(structured)
}

/// Build app-only hydration tool result with plan DAG in `structuredContent.ui`.
pub fn ui_read_plan_tool_result(comp: Value, plan_ux_reflection: Value) -> CallToolResult {
    let mut ui = Map::new();
    ui.insert(
        "kind".into(),
        Value::String(UiPayloadKind::PlanReview.as_str().into()),
    );
    ui.insert("comp".into(), comp);
    ui.insert("plan_ux_reflection".into(), plan_ux_reflection);
    call_tool_result_with_ui(ui)
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
    ui.insert(
        "kind".into(),
        Value::String(UiPayloadKind::RunExplorer.as_str().into()),
    );
    ui.insert("result_delivery".into(), Value::String("inline".into()));
    ui.insert("steps".into(), Value::Array(vec![step]));
    call_tool_result_with_ui(ui)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_agent_present::{AgentContent, PlanTokenRefs, RunTokens};
    use rust_mcp_sdk::schema::{CallToolResult, ContentBlock, TextContent};
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

    fn sample_plan_agent_content() -> String {
        AgentContent::plan(
            &PlanTokenRefs {
                run_ref: "pc0",
                dry_verdict: "ok",
                logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ",
                plan_uri: Some(
                    "plasm://execute/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/s1/plan/00000000-0000-4000-8000-000000000001",
                ),
            },
            "plan ok · 1n 1r → items",
        )
        .render()
    }

    fn first_text(out: &CallToolResult) -> String {
        for block in &out.content {
            if let ContentBlock::TextContent(t) = block {
                return t.text.clone();
            }
        }
        String::new()
    }

    #[test]
    fn finalize_full_apps_emits_ui_lane_without_agent_plasm() {
        let res = CallToolResult::text_content(vec![TextContent::new(
            sample_plan_agent_content(),
            None,
            None,
        )]);
        let out = finalize_mcp_tool_result(
            res,
            sample_dry_tool_meta(),
            McpDeliveryProfile::FullApps,
            None,
        );
        let wire = serde_json::to_value(&out).expect("serialize CallToolResult");
        assert!(wire.pointer("/structuredContent/plasm").is_none());
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
        let text = first_text(&out);
        assert!(text.contains("kind\tplan"));
        assert!(text.contains("run_ref\tpc0"));
        assert!(text.contains("plan ok"));
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
            McpDeliveryProfile::FullApps,
            Some(UiInlinePlanPayload {
                comp: comp.clone(),
                plan_ux_reflection: ux.clone(),
            }),
        );
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/structuredContent/plasm").is_none());
        assert!(wire.pointer("/_meta/plasm/comp").is_none());
        assert_eq!(wire.pointer("/structuredContent/ui/comp"), Some(&comp));
        assert_eq!(
            wire.pointer("/structuredContent/ui/plan_ux_reflection"),
            Some(&ux)
        );
    }

    #[test]
    fn finalize_fused_clean_read_is_run_explorer_without_plan_dag() {
        let mut tool_meta = Map::new();
        let plasm = json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "result_delivery": "inline",
            "steps": [{
                "run_step": 1,
                "return_label": "items",
                "row_count": 1,
                "artifact_uri": "plasm://execute/ph/s/run/prabc",
                "run_id": "prabc",
                "preview_entities": [{ "id": "1" }]
            }]
        });
        tool_meta.insert("plasm".into(), plasm.clone());
        let body = "## items (1 rows)\n\n```tsv\nid\n1\n```";
        let markdown = AgentContent::run(
            RunTokens::from_plasm_obj(plasm.as_object().expect("obj")),
            body,
        )
        .render();
        let comp = json!({ "version": 1, "steps": { "n1": { "kind": "invoke" } }, "bind": { "topo": ["n1"] }, "return": { "kind": "step", "step": "n1" } });
        let ux = json!({ "schema_version": 3, "flow": { "verdict": "clean" }, "steps": [{ "operation": "query" }] });
        let res = CallToolResult::text_content(vec![TextContent::new(markdown, None, None)]);
        // Even if a caller wrongly passes inline plan on a fused read, UI lane stays Run Explorer.
        let out = finalize_mcp_tool_result(
            res,
            tool_meta,
            McpDeliveryProfile::FullApps,
            Some(UiInlinePlanPayload {
                comp: comp.clone(),
                plan_ux_reflection: ux.clone(),
            }),
        );
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/_meta/plasm/comp").is_none());
        assert!(wire.pointer("/structuredContent/plasm").is_none());
        let text = first_text(&out);
        assert!(!text.contains("kind\trun"));
        assert!(!text.contains("artifact_uri\t"));
        assert!(text.contains("## items (1 rows)"));
        assert_eq!(
            wire.pointer("/structuredContent/ui/kind")
                .and_then(|v| v.as_str()),
            Some("run_explorer")
        );
        assert!(wire.pointer("/structuredContent/ui/comp").is_none());
        assert!(wire
            .pointer("/structuredContent/ui/plan_ux_reflection")
            .is_none());
        assert_eq!(
            wire.pointer("/structuredContent/ui/steps/0/return_label")
                .and_then(|v| v.as_str()),
            Some("items")
        );
    }

    #[test]
    fn finalize_attaches_plan_review_for_dry_run() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(
            res,
            sample_dry_tool_meta(),
            McpDeliveryProfile::FullApps,
            None,
        );
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
        let plasm = json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "result_delivery": "inline",
            "steps": [{
                "run_step": 1,
                "return_label": "items",
                "row_count": 1,
                "artifact_uri": "plasm://execute/ph/s/run/prabc",
                "run_id": "prabc",
                "preview_entities": [{ "id": "1" }]
            }]
        });
        tool_meta.insert("plasm".into(), plasm.clone());
        let body = "## items (1 rows)\n\n```tsv\nid\n1\n```";
        let markdown = AgentContent::run(
            RunTokens::from_plasm_obj(plasm.as_object().expect("obj")),
            body,
        )
        .render();
        let res = CallToolResult::text_content(vec![TextContent::new(markdown, None, None)]);
        let out = finalize_mcp_tool_result(res, tool_meta, McpDeliveryProfile::FullApps, None);
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
        assert!(out
            .structured_content
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .is_none());
        let text = first_text(&out);
        assert!(!text.contains("kind\trun"));
        assert!(!text.contains("artifact_uri\t"));
        assert!(text.contains("## items (1 rows)"));
        assert!(!text.contains("run_id\t"));
    }

    #[test]
    fn finalize_tool_fallback_is_content_only() {
        let meta = sample_dry_tool_meta();
        let comp = json!({ "version": 1, "steps": { "n1": { "kind": "invoke" } } });
        let ux = json!({ "schema_version": 3, "steps": [{ "operation": "query" }] });
        let res = CallToolResult::text_content(vec![TextContent::new(
            sample_plan_agent_content(),
            None,
            None,
        )]);
        let out = finalize_mcp_tool_result(
            res,
            meta,
            McpDeliveryProfile::ToolFallback,
            Some(UiInlinePlanPayload {
                comp,
                plan_ux_reflection: ux,
            }),
        );
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/structuredContent").is_none());
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_URI)
        );
        let text = first_text(&out);
        assert!(text.contains("kind\tplan"));
        assert!(text.contains("run_ref\tpc0"));
        assert!(text.contains("plan ok"));
    }

    #[test]
    fn finalize_skips_ui_lane_when_apps_disabled() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(
            res,
            sample_dry_tool_meta(),
            McpDeliveryProfile::ContentOnly,
            None,
        );
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/_meta/ui").is_none());
        assert!(wire.pointer("/structuredContent").is_none());
    }

    #[test]
    fn dual_lane_tool_result_assembles_content_only_context() {
        let plasm = sample_dry_tool_meta()
            .remove("plasm")
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .expect("plasm");
        let out = DualLaneToolResult {
            content: sample_plan_agent_content(),
            plasm_meta: plasm,
            profile: McpDeliveryProfile::ContentOnly,
            inline_plan_ui: None,
        }
        .into_call_tool_result();
        let wire = serde_json::to_value(&out).expect("serialize");
        assert!(wire.pointer("/structuredContent").is_none());
        assert!(wire.pointer("/_meta/ui").is_none());
        assert!(first_text(&out).contains("kind\tplan"));
    }

    #[test]
    fn inline_ui_payload_fits_respects_budget() {
        let small = json!({"a": 1});
        assert!(inline_ui_payload_fits(&small, &small));
        let huge = json!({"x": "y".repeat(INLINE_UI_BYTE_BUDGET)});
        assert!(!inline_ui_payload_fits(&huge, &small));
    }
}
