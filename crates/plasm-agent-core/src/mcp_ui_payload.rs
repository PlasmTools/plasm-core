//! MCP App view payload: attach UI meta and mirror slim agent `_meta.plasm` into `structuredContent`.

use rust_mcp_sdk::schema::CallToolResult;
use serde_json::{Map, Value};

/// Sole MCP tool-result exit: merge `_meta`, attach MCP App UI, mirror agent payload.
pub fn finalize_mcp_tool_result(
    res: CallToolResult,
    mut tool_meta: Map<String, Value>,
    agent_plan_text: Option<&str>,
) -> CallToolResult {
    crate::mcp_app::attach_mcp_app_ui_on_tool_meta(&mut tool_meta);
    let res = res.with_meta(Some(tool_meta));
    let res = mirror_plasm_structured_content(res);
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

/// Copy slim agent `_meta.plasm` into `structuredContent.plasm` (no UI DAG / run snapshot steps).
///
/// Run Explorer and Plan Review read full `steps` / plan DAG from `_meta.plasm` (UI channel) or
/// `resources/read` on `plan_uri` / run artifact URIs — not from agent `structuredContent`.
pub fn mirror_plasm_structured_content(res: CallToolResult) -> CallToolResult {
    let Some(meta) = res.meta.as_ref() else {
        return res;
    };
    let Some(plasm) = meta.get("plasm") else {
        return res;
    };
    let structured_plasm = agent_structured_plasm_mirror(plasm);
    let mut structured = Map::new();
    structured.insert("plasm".to_string(), structured_plasm);
    res.with_structured_content(structured)
}

const AGENT_STRUCTURED_DENY: &[&str] = &["comp", "plan_ux_reflection", "program", "steps", "plan"];

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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{CallToolResult, TextContent};
    use serde_json::json;

    fn minimal_plan_ux_reflection_v3() -> serde_json::Value {
        json!({
            "schema_version": 3,
            "layout": "sequential",
            "steps": [],
            "review": { "verdict": "ok", "write_count": 0, "read_count": 0 },
            "flow": {
                "schema_version": 1,
                "verdict": "clean",
                "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
                "violations": [],
                "trace": []
            }
        })
    }

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
            json!("plasm://session/l_AAAAAAAAQACAAAAAAAAAAQ/p/1"),
        );
        let mut meta = Map::new();
        meta.insert("plasm".into(), Value::Object(agent));
        meta
    }

    const SAMPLE_PLAN_TEXT: &str = "plan ok · 1n 1r → items\n\n01 items     query LangItem";

    #[test]
    fn finalize_dry_run_structured_content_is_slim_agent_tokens_only() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta(), Some(SAMPLE_PLAN_TEXT));
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
        assert!(wire.pointer("/structuredContent/ui").is_none());
        assert!(wire.pointer("/structuredContent/plasm/comp").is_none());
        assert!(wire
            .pointer("/structuredContent/plasm/plan_ux_reflection")
            .is_none());
        assert!(wire.pointer("/structuredContent/plasm/program").is_none());
        assert!(wire.pointer("/structuredContent/plasm/steps").is_none());
        assert_eq!(
            wire.pointer("/structuredContent/plasm/plan_uri")
                .and_then(|v| v.as_str()),
            Some("plasm://session/l_AAAAAAAAQACAAAAAAAAAAQ/p/1")
        );
    }

    #[test]
    fn finalize_attaches_plan_review_for_dry_run() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta(), None);
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
        let out = finalize_mcp_tool_result(res, tool_meta, None);
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_URI)
        );
    }

    #[test]
    fn finalize_legacy_plan_without_dry_run_attaches_no_ui() {
        let mut tool_meta = Map::new();
        tool_meta.insert("plasm".into(), json!({ "plan": { "nodes": [] } }));
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, tool_meta, None);
        assert!(out.meta.as_ref().and_then(|m| m.get("ui")).is_none());
    }

    #[test]
    fn mirror_live_run_strips_steps_from_structured_content() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
                "steps": [{
                    "run_step": 1,
                    "return_label": "issue",
                    "row_count": 1,
                    "artifact_uri": "plasm://session/l_AAAAAAAAQACAAAAAAAAAAQ/r/1",
                    "run_id": "prabc"
                }]
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)])
            .with_meta(Some(meta));
        let out = mirror_plasm_structured_content(res);
        assert!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("steps"))
                .is_none(),
            "agent structuredContent must not mirror snapshot steps"
        );
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("logical_session_ref"))
                .and_then(|v| v.as_str()),
            Some("l_AAAAAAAAQACAAAAAAAAAAQ")
        );
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("steps"))
                .and_then(|s| s.as_array())
                .map(|a| a.len()),
            Some(1),
            "_meta.plasm.steps remain for Run Explorer"
        );
    }

    #[test]
    fn mirror_noop_without_meta_plasm() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = mirror_plasm_structured_content(res);
        assert!(out.structured_content.is_none());
    }
}
