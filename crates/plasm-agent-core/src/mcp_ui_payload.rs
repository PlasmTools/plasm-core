//! MCP App view payload: attach UI meta and mirror `_meta.plasm` into `structuredContent`.

use rust_mcp_sdk::schema::CallToolResult;
use serde_json::{Map, Value};

/// Sole MCP tool-result exit: build `_meta`, attach MCP App UI, mirror render payload.
pub fn finalize_mcp_tool_result(
    res: CallToolResult,
    plasm_obj: Map<String, Value>,
) -> CallToolResult {
    let mut meta = Map::new();
    meta.insert("plasm".into(), Value::Object(plasm_obj));
    crate::mcp_app::attach_mcp_app_ui_on_tool_meta(&mut meta);
    let res = res.with_meta(Some(meta));
    mirror_plasm_structured_content(res)
}

/// Copy `_meta.plasm` into `structuredContent.plasm` for hosts that strip `_meta`.
pub fn mirror_plasm_structured_content(res: CallToolResult) -> CallToolResult {
    let Some(meta) = res.meta.as_ref() else {
        return res;
    };
    let Some(plasm) = meta.get("plasm") else {
        return res;
    };
    let mut structured = Map::new();
    structured.insert("plasm".to_string(), plasm.clone());
    res.with_structured_content(structured)
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

    #[test]
    fn finalize_attaches_plan_review_and_mirrors_comp() {
        let mut plasm_obj = Map::new();
        plasm_obj.insert("dry_run".into(), json!(true));
        plasm_obj.insert(
            "comp".into(),
            json!({
                "steps": { "n1": {} },
                "bind": { "topo": ["n1"] },
                "return": { "kind": "step", "step": "n1" }
            }),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, plasm_obj);
        assert_eq!(
            out.meta
                .as_ref()
                .and_then(|m| m.get("ui"))
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_URI)
        );
        assert_eq!(
            out.structured_content
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("comp")),
            out.meta
                .as_ref()
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("comp"))
        );
    }

    #[test]
    fn finalize_attaches_run_explorer_for_live_steps() {
        let mut plasm_obj = Map::new();
        plasm_obj.insert(
            "steps".into(),
            json!([{ "run_step": 1, "return_label": "items", "row_count": 1 }]),
        );
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, plasm_obj);
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
        let mut plasm_obj = Map::new();
        plasm_obj.insert("dry_run".into(), json!(true));
        plasm_obj.insert("plan".into(), json!({ "nodes": [] }));
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = finalize_mcp_tool_result(res, plasm_obj);
        assert!(out.meta.as_ref().and_then(|m| m.get("ui")).is_none());
    }

    #[test]
    fn mirror_plasm_structured_content_copies_meta_plasm() {
        let mut meta = Map::new();
        meta.insert(
            "plasm".to_string(),
            json!({
                "dry_run": true,
                "comp": {
                    "steps": { "n1": {} },
                    "bind": { "topo": ["n1"] },
                    "return": { "kind": "step", "step": "n1" }
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
                .and_then(|p| p.get("comp")),
            Some(&json!({
                "steps": { "n1": {} },
                "bind": { "topo": ["n1"] },
                "return": { "kind": "step", "step": "n1" }
            }))
        );
    }

    #[test]
    fn mirror_noop_without_meta_plasm() {
        let res = CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]);
        let out = mirror_plasm_structured_content(res);
        assert!(out.structured_content.is_none());
    }
}
