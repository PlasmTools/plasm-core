//! Registry for MCP App bundles (plan review, workflow, …): HTTP routes, resource read, UI meta.

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_mcp_sdk::schema::TextResourceContents;
use serde_json::Map;

/// Static assets + routing for one MCP App bundle.
pub struct McpAppBundle {
    pub uri: &'static str,
    pub mime: &'static str,
    pub mcp_inline_html: &'static str,
    pub http_page_html: &'static str,
    pub http_page_js: &'static str,
    pub shell_html: &'static str,
    pub shell_js: &'static str,
    /// HTTP path prefix without trailing slash (e.g. `/v1/plan/ui`).
    pub path_prefix: &'static str,
    /// Metrics label for `resources/read`.
    pub resource_metric: &'static str,
}

pub const PLAN_REVIEW: McpAppBundle = McpAppBundle {
    uri: crate::plan_ui_mcp::PLAN_REVIEW_UI_URI,
    mime: crate::plan_ui_mcp::PLAN_REVIEW_UI_MIME,
    mcp_inline_html: crate::plan_ui_mcp::PLAN_REVIEW_UI_HTML,
    http_page_html: crate::plan_ui_mcp::PLAN_UI_PAGE_HTML,
    http_page_js: crate::plan_ui_mcp::PLAN_UI_JS,
    shell_html: crate::plan_ui_mcp::PLAN_SHELL_HTML,
    shell_js: crate::plan_ui_mcp::PLAN_SHELL_JS,
    path_prefix: "/v1/plan/ui",
    resource_metric: "plan_review_ui",
};

pub const WORKFLOW: McpAppBundle = McpAppBundle {
    uri: crate::workflow_mcp::WORKFLOW_UI_URI,
    mime: crate::workflow_mcp::WORKFLOW_UI_MIME,
    mcp_inline_html: crate::workflow_mcp::WORKFLOW_UI_HTML,
    http_page_html: crate::workflow_mcp::WORKFLOW_UI_PAGE_HTML,
    http_page_js: crate::workflow_mcp::WORKFLOW_UI_JS,
    shell_html: crate::workflow_mcp::WORKFLOW_SHELL_HTML,
    shell_js: crate::workflow_mcp::WORKFLOW_SHELL_JS,
    path_prefix: "/v1/workflows/ui",
    resource_metric: "workflow_ui",
};

pub const RUN_EXPLORER: McpAppBundle = McpAppBundle {
    uri: crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_URI,
    mime: crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_MIME,
    mcp_inline_html: crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_HTML,
    http_page_html: crate::run_explorer_ui_mcp::RUN_UI_PAGE_HTML,
    http_page_js: crate::run_explorer_ui_mcp::RUN_UI_JS,
    shell_html: crate::run_explorer_ui_mcp::RUN_SHELL_HTML,
    shell_js: crate::run_explorer_ui_mcp::RUN_SHELL_JS,
    path_prefix: "/v1/run/ui",
    resource_metric: "run_explorer_ui",
};

static BUNDLES: &[&McpAppBundle] = &[&PLAN_REVIEW, &WORKFLOW, &RUN_EXPLORER];

/// CSP metadata for inline MCP App resources (same-origin API fetch in HTTP standalone mode).
pub fn resource_read_content_meta() -> Map<String, serde_json::Value> {
    serde_json::json!({
        "ui": {
            "csp": {
                "connectDomains": [
                    "http://127.0.0.1:*",
                    "http://localhost:*",
                    "https://127.0.0.1:*",
                    "https://localhost:*",
                    "https://platform.plasm.tools",
                    "https://*.plasm.tools"
                ]
            }
        }
    })
    .as_object()
    .cloned()
    .expect("resource csp meta")
}

pub fn bundle_for_uri(uri: &str) -> Option<&'static McpAppBundle> {
    BUNDLES.iter().copied().find(|b| b.uri == uri)
}

pub fn read_resource_text(
    uri: &str,
) -> Option<(TextResourceContents, Map<String, serde_json::Value>)> {
    let bundle = bundle_for_uri(uri)?;
    Some((
        TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some(bundle.mime.into()),
            text: bundle.mcp_inline_html.to_string(),
            meta: Some(resource_read_content_meta()),
        },
        resource_read_content_meta(),
    ))
}

fn plasm_object(meta: &Map<String, serde_json::Value>) -> Option<&Map<String, serde_json::Value>> {
    meta.get("plasm").and_then(|p| p.as_object())
}

fn plasm_dry_run(plasm: &Map<String, serde_json::Value>) -> bool {
    plasm
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn plasm_has_comp(plasm: &Map<String, serde_json::Value>) -> bool {
    plasm.get("comp").is_some()
}

/// Attach MCP App mount metadata from `_meta.plasm` comp-only wire (no legacy `plan`).
pub fn attach_mcp_app_ui_on_tool_meta(meta: &mut Map<String, serde_json::Value>) {
    let Some(plasm) = plasm_object(meta) else {
        return;
    };
    if plasm_dry_run(plasm) && plasm_has_comp(plasm) {
        meta.insert(
            "ui".into(),
            serde_json::json!({
                "resourceUri": PLAN_REVIEW.uri
            }),
        );
        return;
    }
    let has_steps = plasm
        .get("steps")
        .and_then(|s| s.as_array())
        .is_some_and(|steps| !steps.is_empty());
    if has_steps && !plasm_dry_run(plasm) {
        meta.insert(
            "ui".into(),
            serde_json::json!({
                "resourceUri": RUN_EXPLORER.uri
            }),
        );
    }
}

pub fn mount_bundle(bundle: &'static McpAppBundle) -> Router {
    mount_bundle_routes(Router::new(), bundle)
}

fn mount_bundle_routes(router: Router, bundle: &'static McpAppBundle) -> Router {
    let prefix = bundle.path_prefix;
    let shell_path = prefix.to_string();
    let shell_js_path = format!("{prefix}/shell.js");
    let app_path = format!("{prefix}/app");
    let app_js_path = format!("{prefix}/app.js");

    let shell_html = bundle.shell_html;
    let shell_js = bundle.shell_js;
    let page_html = bundle.http_page_html;
    let page_js = bundle.http_page_js;
    let app_mime = bundle.mime;

    router
        .route(
            shell_path.as_str(),
            get(move || serve_html(shell_html, "text/html; charset=utf-8")),
        )
        .route(
            shell_js_path.as_str(),
            get(move || serve_html(shell_js, "text/javascript; charset=utf-8")),
        )
        .route(
            app_path.as_str(),
            get(move || serve_html(page_html, app_mime)),
        )
        .route(
            app_js_path.as_str(),
            get(move || serve_html(page_js, "text/javascript; charset=utf-8")),
        )
}

async fn serve_html(body: &'static str, content_type: &'static str) -> Response {
    (
        [(CONTENT_TYPE, content_type), (CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_resolve_by_uri() {
        assert!(bundle_for_uri(PLAN_REVIEW.uri).is_some());
        assert!(bundle_for_uri(WORKFLOW.uri).is_some());
        assert!(bundle_for_uri(RUN_EXPLORER.uri).is_some());
        assert!(bundle_for_uri("ui://plasm/missing").is_none());
    }

    #[test]
    fn attach_mcp_app_ui_plan_review_on_dry_run_comp() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::json!({ "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ" }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert!(meta.get("ui").is_none());

        meta.insert(
            "plasm".into(),
            serde_json::json!({
                "dry_run": true,
                "comp": { "steps": { "n1": {} }, "bind": { "topo": ["n1"] }, "return": { "kind": "step", "step": "n1" } }
            }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert_eq!(
            meta.get("ui")
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(PLAN_REVIEW.uri)
        );

        meta.remove("ui");
        meta.insert(
            "plasm".into(),
            serde_json::json!({
                "dry_run": true,
                "plan": { "nodes": [] },
                "steps": [{ "run_id": "prabc" }]
            }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert!(
            meta.get("ui").is_none(),
            "legacy plan without comp must not attach plan-review"
        );
    }

    #[test]
    fn attach_mcp_app_ui_run_explorer_only_for_live_steps() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::json!({ "steps": [{ "run_id": "prabc" }] }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert_eq!(
            meta.get("ui")
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(RUN_EXPLORER.uri)
        );

        meta.insert(
            "plasm".into(),
            serde_json::json!({
                "dry_run": true,
                "steps": [{ "run_id": "prabc" }]
            }),
        );
        meta.remove("ui");
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert!(
            meta.get("ui").is_none(),
            "dry-run payload — run-explorer must not attach"
        );
    }
}
