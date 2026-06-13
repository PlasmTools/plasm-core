//! MCP App view payload: mirror `_meta.plasm` into `structuredContent` for hosts that strip `_meta`.

use rust_mcp_sdk::schema::CallToolResult;
use serde_json::Map;

/// After `with_meta`, copy `_meta.plasm` into `structuredContent.plasm` on the same response.
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::{CallToolResult, TextContent};
    use serde_json::json;

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
