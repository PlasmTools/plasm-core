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
            text: inject_mcp_app_client_config(bundle.mcp_inline_html),
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

fn plasm_is_live_operation_pending(plasm: &Map<String, serde_json::Value>) -> bool {
    if plasm_dry_run(plasm) || plasm_has_comp(plasm) {
        return false;
    }
    let Some(continuity) = plasm.get("continuity").and_then(|c| c.as_object()) else {
        return false;
    };
    let has_handle = continuity
        .get("h")
        .and_then(|h| h.as_str())
        .is_some_and(|s| !s.is_empty());
    if !has_handle {
        return false;
    }
    if plasm.get("op").and_then(|o| o.as_object()).is_some() {
        return true;
    }
    plasm.get("auto_async").and_then(|v| v.as_bool()) == Some(true)
}

fn plasm_ui_payload(meta: &Map<String, serde_json::Value>) -> Option<&Map<String, serde_json::Value>> {
    meta.get("ui")
        .and_then(|u| u.as_object())
        .and_then(|u| u.get("plasm"))
        .and_then(|p| p.as_object())
}

fn plasm_has_ui_comp(meta: &Map<String, serde_json::Value>) -> bool {
    plasm_ui_payload(meta)
        .and_then(|p| p.get("comp"))
        .is_some()
}

/// Attach MCP App mount metadata from dry-run UI payload or live `steps`.
pub fn attach_mcp_app_ui_on_tool_meta(meta: &mut Map<String, serde_json::Value>) {
    let Some(plasm) = plasm_object(meta) else {
        return;
    };
    if plasm_dry_run(plasm) && plasm_has_ui_comp(meta) {
        let mut ui = meta
            .get("ui")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        ui.entry("resourceUri")
            .or_insert(serde_json::json!(PLAN_REVIEW.uri));
        meta.insert("ui".into(), serde_json::Value::Object(ui));
        return;
    }
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
    if (has_steps || plasm_is_live_operation_pending(plasm)) && !plasm_dry_run(plasm) {
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

const MCP_SHELL_CONFIG_MARKER: &str = "<!-- plasm-mcp-config -->";

/// HTTP API origin for MCP App iframes (Cursor in-chat is not same-origin with `plasm-mcp`).
pub fn mcp_public_api_origin() -> Option<String> {
    std::env::var("PLASM_MCP_PUBLIC_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|base| url::Url::parse(base.trim()).ok())
        .map(|u| u.origin().ascii_serialization())
}

fn mcp_app_client_config_script() -> String {
    let stream = mcp_public_stream_path();
    let mut script = format!(r#"<script>window.__PLASM_MCP_STREAM_PATH__={stream:?};"#,);
    if let Some(origin) = mcp_public_api_origin() {
        script.push_str(&format!("window.__PLASM_API_ORIGIN__={origin:?};"));
    }
    script.push_str("</script>");
    script
}

/// Inject MCP App client globals into inline resource HTML or shell pages.
pub fn inject_mcp_app_client_config(html: &str) -> String {
    let inject = mcp_app_client_config_script();
    if html.contains(MCP_SHELL_CONFIG_MARKER) {
        return html.replace(MCP_SHELL_CONFIG_MARKER, &inject);
    }
    if let Some(head_end) = html.find("</head>") {
        let mut out = String::with_capacity(html.len() + inject.len() + 8);
        out.push_str(&html[..head_end]);
        out.push_str(&inject);
        out.push_str(&html[head_end..]);
        return out;
    }
    format!("{inject}{html}")
}

/// Streamable HTTP path for browser MCP App shells (`/mcp` vs ingress `/plasm/mcp`).
pub fn mcp_public_stream_path() -> String {
    std::env::var("PLASM_MCP_PUBLIC_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|base| url::Url::parse(base.trim()).ok())
        .map(|u| {
            let mut path = u.path().trim_end_matches('/').to_string();
            if path.is_empty() {
                "/mcp".into()
            } else {
                path.push_str("/mcp");
                path
            }
        })
        .unwrap_or_else(|| "/mcp".into())
}

fn shell_html_with_mcp_config(body: &str) -> String {
    inject_mcp_app_client_config(body)
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
            get(move || serve_shell_html(shell_html)),
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

async fn serve_shell_html(body: &'static str) -> Response {
    (
        [
            (CONTENT_TYPE, "text/html; charset=utf-8"),
            (CACHE_CONTROL, "no-store"),
        ],
        shell_html_with_mcp_config(body),
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
    fn attach_mcp_app_ui_plan_review_on_dry_run_ui_comp() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::json!({ "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ" }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert!(meta.get("ui").is_none());

        meta.insert("plasm".into(), serde_json::json!({ "dry_run": true }));
        meta.insert(
            "ui".into(),
            serde_json::json!({
                "plasm": {
                    "comp": { "steps": { "n1": {} }, "bind": { "topo": ["n1"] }, "return": { "kind": "step", "step": "n1" } }
                }
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

    fn with_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().expect("env test lock");
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn mcp_public_stream_path_defaults_to_root_mcp() {
        with_env("PLASM_MCP_PUBLIC_BASE_URL", None, || {
            assert_eq!(super::mcp_public_stream_path(), "/mcp");
        });
        with_env(
            "PLASM_MCP_PUBLIC_BASE_URL",
            Some("https://platform.plasm.tools/plasm"),
            || {
                assert_eq!(super::mcp_public_stream_path(), "/plasm/mcp");
            },
        );
    }

    #[test]
    fn shell_html_injects_mcp_stream_path() {
        with_env(
            "PLASM_MCP_PUBLIC_BASE_URL",
            Some("https://platform.plasm.tools/plasm"),
            || {
                let html = super::shell_html_with_mcp_config(
                    "<html><head><!-- plasm-mcp-config --></head></html>",
                );
                assert!(html.contains(r#"window.__PLASM_MCP_STREAM_PATH__="/plasm/mcp""#));
                assert!(
                    html.contains(r#"window.__PLASM_API_ORIGIN__="https://platform.plasm.tools""#)
                );
            },
        );
    }

    #[test]
    fn inline_resource_html_injects_api_origin_after_head() {
        with_env(
            "PLASM_MCP_PUBLIC_BASE_URL",
            Some("https://platform.plasm.tools/plasm"),
            || {
                let html = super::inject_mcp_app_client_config(
                    "<!DOCTYPE html><html><head><title>x</title></head><body></body></html>",
                );
                assert!(
                    html.contains(r#"window.__PLASM_API_ORIGIN__="https://platform.plasm.tools""#)
                );
                assert!(html
                    .find("</head>")
                    .is_some_and(|i| html[..i].contains("__PLASM_API_ORIGIN__")));
            },
        );
    }

    #[test]
    fn attach_mcp_app_ui_run_explorer_on_async_accept() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::json!({
                "logical_session_ref": "l_r-b8dYGwR4SOf_Scx27Xvw",
                "auto_async": true,
                "continuity": { "p": "running", "h": "l_r-b8dYGwR4SOf_Scx27Xvw_o1" },
                "op": { "n": 1, "+": 1, "c": "pc1" },
                "dry_verdict": "review"
            }),
        );
        attach_mcp_app_ui_on_tool_meta(&mut meta);
        assert_eq!(
            meta.get("ui")
                .and_then(|u| u.get("resourceUri"))
                .and_then(|v| v.as_str()),
            Some(RUN_EXPLORER.uri)
        );
    }
}
