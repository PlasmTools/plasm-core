//! MCP App view payload: attach UI meta and mirror agent-facing `_meta.plasm` into `structuredContent`.

use rust_mcp_sdk::schema::CallToolResult;
use serde_json::{Map, Value};

/// Sole MCP tool-result exit: merge `_meta`, attach MCP App UI, mirror agent payload.
pub fn finalize_mcp_tool_result(
    res: CallToolResult,
    mut tool_meta: Map<String, Value>,
) -> CallToolResult {
    crate::mcp_app::attach_mcp_app_ui_on_tool_meta(&mut tool_meta);
    let res = res.with_meta(Some(tool_meta));
    mirror_plasm_structured_content(res)
}

/// Copy agent `_meta.plasm` into `structuredContent.plasm`, plus UI DAG fields when present.
///
/// Cursor and other MCP App hosts often forward `structuredContent` but strip nested `_meta.ui`;
/// mirror `_meta.ui.plasm` (`comp`, `plan_ux_reflection`) into `structuredContent.plasm` so Plan
/// Review can render without relying on host `_meta.ui` forwarding.
pub fn mirror_plasm_structured_content(res: CallToolResult) -> CallToolResult {
    let Some(meta) = res.meta.as_ref() else {
        return res;
    };
    let Some(plasm) = meta.get("plasm") else {
        return res;
    };
    let mut structured_plasm = agent_structured_plasm_mirror(plasm);
    mirror_ui_plasm_into_structured(meta, &mut structured_plasm);
    let mut structured = Map::new();
    structured.insert("plasm".to_string(), structured_plasm);
    res.with_structured_content(structured)
}

fn mirror_ui_plasm_into_structured(meta: &Map<String, Value>, structured_plasm: &mut Value) {
    let Some(ui_plasm) = meta
        .get("ui")
        .and_then(|u| u.get("plasm"))
        .and_then(|p| p.as_object())
    else {
        return;
    };
    let Some(out) = structured_plasm.as_object_mut() else {
        return;
    };
    for key in ["comp", "plan_ux_reflection"] {
        if let Some(v) = ui_plasm.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
}

fn agent_structured_plasm_mirror(plasm: &Value) -> Value {
    let Some(obj) = plasm.as_object() else {
        return plasm.clone();
    };
    if obj.get("dry_run").and_then(|v| v.as_bool()) != Some(true) {
        return plasm.clone();
    }
    const KEEP: &[&str] = &[
        "dry_run",
        "logical_session_ref",
        "run_ref",
        "dry_verdict",
        "dry_review",
        "domain_revision",
        "projection_warning",
        "session_notes",
    ];
    let mut out = Map::new();
    for key in KEEP {
        if let Some(v) = obj.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
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
        let mut ui_plasm = Map::new();
        ui_plasm.insert(
            "comp".into(),
            json!({
                "version": 1,
                "steps": { "n1": { "kind": "invoke", "operation": "query e1" } },
                "bind": { "topo": ["n1"] },
                "return": { "kind": "step", "step": "n1" }
            }),
        );
        ui_plasm.insert("plan_ux_reflection".into(), minimal_plan_ux_reflection_v3());
        let mut meta = Map::new();
        meta.insert("plasm".into(), Value::Object(agent));
        meta.insert("ui".into(), json!({ "plasm": ui_plasm }));
        meta
    }

    #[test]
    fn finalize_dry_run_structured_content_mirrors_ui_plan_for_cursor_hosts() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta());
        let wire = serde_json::to_value(&out).expect("serialize CallToolResult");
        assert_eq!(
            wire.pointer("/structuredContent/plasm/run_ref")
                .and_then(|v| v.as_str()),
            Some("pc0")
        );
        assert_eq!(
            wire.pointer("/structuredContent/plasm/comp/bind/topo")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1),
            "Cursor-style hosts need comp in structuredContent when _meta.ui is stripped"
        );
        assert!(wire
            .pointer("/structuredContent/plasm/plan_ux_reflection/schema_version")
            .is_some());
        assert!(wire.pointer("/structuredContent/plasm/program").is_none());
        assert_eq!(
            wire.pointer("/_meta/ui/plasm/comp/bind/topo")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            wire.pointer("/_meta/ui/resourceUri")
                .and_then(|v| v.as_str()),
            Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_URI)
        );
        assert!(wire.pointer("/_meta/plasm/comp").is_none());
    }

    #[test]
    fn finalize_attaches_plan_review_for_ui_comp() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, sample_dry_tool_meta());
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
        let out = finalize_mcp_tool_result(res, tool_meta);
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
    fn finalize_legacy_plan_without_comp_attaches_no_ui() {
        let mut tool_meta = Map::new();
        tool_meta.insert(
            "plasm".into(),
            json!({ "dry_run": true, "plan": { "nodes": [] } }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, tool_meta);
        assert!(out.meta.as_ref().and_then(|m| m.get("ui")).is_none());
    }

    #[test]
    fn mirror_dry_run_strips_agent_comp_but_keeps_ui_comp_in_structured() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "dry_run": true,
                "run_ref": "pc0",
                "comp": {
                    "steps": { "n1": {} },
                    "bind": { "topo": ["n1"] },
                    "return": { "kind": "step", "step": "n1" }
                }
            }),
        );
        meta.insert(
            "ui".into(),
            json!({
                "plasm": {
                    "comp": {
                        "steps": { "n1": { "kind": "invoke" } },
                        "bind": { "topo": ["n1"] },
                        "return": { "kind": "step", "step": "n1" }
                    },
                    "plan_ux_reflection": minimal_plan_ux_reflection_v3()
                }
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)])
            .with_meta(Some(meta));
        let out = mirror_plasm_structured_content(res);
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("run_ref"))
                .and_then(|v| v.as_str()),
            Some("pc0")
        );
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("comp"))
                .and_then(|c| c.get("steps"))
                .and_then(|s| s.get("n1"))
                .and_then(|n| n.get("kind"))
                .and_then(|v| v.as_str()),
            Some("invoke"),
            "UI comp must mirror into structuredContent for lossy hosts"
        );
    }

    #[test]
    fn mirror_live_run_copies_full_plasm() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "steps": [{ "run_step": 1, "return_label": "items" }]
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)])
            .with_meta(Some(meta));
        let out = mirror_plasm_structured_content(res);
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("steps"))
                .and_then(|s| s.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn mirror_noop_without_meta_plasm() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = mirror_plasm_structured_content(res);
        assert!(out.structured_content.is_none());
    }
}
