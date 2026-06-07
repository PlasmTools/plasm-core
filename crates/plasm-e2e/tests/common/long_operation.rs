//! Hermit + unified HTTP/MCP server harness for long-operation E2E tests.

use std::time::Duration;

use plasm_agent::http::{serve_discovery_execute_and_mcp_unified, DiscoveryHttpServeOpts};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use reqwest::header::{HeaderMap, LOCATION};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;

use super::hermit_lang_matrix;
use super::language_matrix::{self, MATRIX_ENTRY_ID};

const MCP_PROTOCOL: &str = "2025-11-25";
pub const UNBOUNDED_LANG_ITEM: &str = "LangItem";
pub const BOUNDED_LANG_ITEM: &str = "LangItem.limit(2)";
/// Paginated list — slow enough to observe Running/cancel without unbounded review fanout.
pub const SLOW_LANG_ITEM: &str = "LangItem.page_size(1).limit(10)";

async fn spawn_unified_server() -> (String, JoinHandle<()>) {
    let hermit = hermit_lang_matrix::language_matrix_hermit_base_url().await.clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(hermit),
        ..Default::default()
    })
    .expect("execution engine");
    let st = language_matrix::matrix_host_state(engine, cgs);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unified server");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = tokio::spawn(async move {
        serve_discovery_execute_and_mcp_unified(
            listener,
            st,
            DiscoveryHttpServeOpts {
                emit_stderr_route_help: false,
            },
        )
        .await
        .ok();
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    (base, handle)
}

static SHARED_SERVER: OnceCell<(String, JoinHandle<()>)> = OnceCell::const_new();

async fn shared_server_base_url() -> String {
    SHARED_SERVER
        .get_or_init(|| async { spawn_unified_server().await })
        .await
        .0
        .clone()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Http,
    Mcp,
}

#[derive(Clone, Debug)]
pub struct RunOpts {
    pub wait: bool,
    pub force: bool,
    pub plan_commit_ref: Option<String>,
    pub plan_only: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            wait: true,
            force: false,
            plan_commit_ref: None,
            plan_only: false,
        }
    }
}

pub struct LongOpFixture {
    pub base_url: String,
    client: reqwest::Client,
    pub http_prompt_hash: String,
    pub http_session_id: String,
    pub mcp_transport_id: String,
    pub logical_session_ref: String,
    rpc_id: std::sync::atomic::AtomicU64,
}

impl LongOpFixture {
    pub async fn setup() -> Self {
        let base_url = shared_server_base_url().await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        let (http_prompt_hash, http_session_id) =
            http_open_session(&client, &base_url).await.expect("http open session");
        let mcp_transport_id = mcp_initialize(&client, &base_url).await;
        let logical_session_ref =
            mcp_plasm_context(&client, &base_url, &mcp_transport_id).await;
        Self {
            base_url,
            client,
            http_prompt_hash,
            http_session_id,
            mcp_transport_id,
            logical_session_ref,
            rpc_id: std::sync::atomic::AtomicU64::new(10),
        }
    }

    fn next_rpc_id(&self) -> u64 {
        self.rpc_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub async fn plan_dry(&self, surface: Surface, program: &str) -> Value {
        match surface {
            Surface::Http => {
                http_post_program(
                    &self.client,
                    &self.base_url,
                    &self.http_prompt_hash,
                    &self.http_session_id,
                    program,
                    RunOpts {
                        plan_only: true,
                        ..Default::default()
                    },
                )
                .await
                .expect("http plan dry")
            }
            Surface::Mcp => {
                let body = mcp_tool_call(
                    &self.client,
                    &self.base_url,
                    &self.mcp_transport_id,
                    self.next_rpc_id(),
                    "plasm",
                    json!({
                        "logical_session_ref": self.logical_session_ref,
                        "program": program,
                    }),
                )
                .await;
                if let Some(err) = body.get("_run_error").and_then(|v| v.as_str()) {
                    panic!("mcp plasm dry-run failed: {err}");
                }
                body
            }
        }
    }

    pub async fn run_program(
        &self,
        surface: Surface,
        program: &str,
        opts: RunOpts,
    ) -> Result<Value, String> {
        match surface {
            Surface::Http => {
                http_post_program(
                    &self.client,
                    &self.base_url,
                    &self.http_prompt_hash,
                    &self.http_session_id,
                    program,
                    opts,
                )
                .await
            }
            Surface::Mcp => {
                let mut args = json!({
                    "logical_session_ref": self.logical_session_ref,
                    "program": program,
                    "wait": opts.wait,
                    "force": opts.force,
                });
                if let Some(pc) = opts.plan_commit_ref {
                    args["plan_commit_ref"] = json!(pc);
                }
                Ok(
                    mcp_tool_call(
                        &self.client,
                        &self.base_url,
                        &self.mcp_transport_id,
                        self.next_rpc_id(),
                        "plasm_run",
                        args,
                    )
                    .await,
                )
                .and_then(|body| {
                    if let Some(err) = body.get("_run_error").and_then(|v| v.as_str()) {
                        Err(err.to_string())
                    } else {
                        Ok(body)
                    }
                })
            }
        }
    }

    /// Best-effort cancel of background async ops so orphaned tasks do not hit Hermit after the test ends.
    pub async fn cleanup(&self) {
        for surface in [Surface::Http, Surface::Mcp] {
            let _ = self
                .run_program(surface, "cancel(s0_o1)", RunOpts::default())
                .await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub async fn http_open_session(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(String, String), String> {
    let resp = client
        .post(format!("{base_url}/execute"))
        .header("accept", "application/json")
        .json(&json!({
            "entry_id": MATRIX_ENTRY_ID,
            "entities": ["LangItem"],
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() != StatusCode::SEE_OTHER {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("expected 303 create, got {status}: {body}"));
    }
    let loc = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "missing Location header".to_string())?
        .to_string();
    let get = client
        .get(format!("{base_url}{loc}"))
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let body: Value = get.json().await.map_err(|e| e.to_string())?;
    let ph = body
        .get("prompt_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing prompt_hash".to_string())?
        .to_string();
    let sid = body
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing session".to_string())?
        .to_string();
    Ok((ph, sid))
}

pub async fn http_post_program(
    client: &reqwest::Client,
    base_url: &str,
    prompt_hash: &str,
    session_id: &str,
    program: &str,
    opts: RunOpts,
) -> Result<Value, String> {
    let mut url = format!("{base_url}/execute/{prompt_hash}/{session_id}");
    let mut q: Vec<String> = Vec::new();
    if !opts.wait {
        q.push("wait=false".into());
    }
    if opts.force {
        q.push("force=true".into());
    }
    if opts.plan_only {
        q.push("mode=plan".into());
    }
    if let Some(ref pc) = opts.plan_commit_ref {
        q.push(format!("plan_commit_ref={pc}"));
    }
    if !q.is_empty() {
        url.push('?');
        url.push_str(&q.join("&"));
    }
    let resp = client
        .post(&url)
        .header("accept", "application/json")
        .header("content-type", "text/plain; charset=utf-8")
        .body(program.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(http_error_detail(&body))
    }
}

fn http_error_detail(body: &Value) -> String {
    body.get("detail")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("title").and_then(|v| v.as_str()))
        .unwrap_or("http error")
        .to_string()
}

pub async fn mcp_initialize(client: &reqwest::Client, base_url: &str) -> String {
    let (headers, _) = mcp_post(
        client,
        base_url,
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "long-op-e2e", "version": "0" }
            }
        }),
    )
    .await;
    let sid = mcp_session_id_from_headers(&headers).expect("mcp-session-id from initialize");
    mcp_post(
        client,
        base_url,
        Some(&sid),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await;
    sid
}

pub async fn mcp_plasm_context(
    client: &reqwest::Client,
    base_url: &str,
    mcp_session_id: &str,
) -> String {
    let body = mcp_tool_call(
        client,
        base_url,
        mcp_session_id,
        4,
        "plasm_context",
        json!({
            "intent": "long-operation e2e LangItem reads",
            "seeds": [{ "api": MATRIX_ENTRY_ID, "entity": "LangItem" }],
        }),
    )
    .await;
    let meta = plasm_meta(&body);
    meta.get("logical_session_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("s0")
        .to_string()
}

async fn mcp_tool_call(
    client: &reqwest::Client,
    base_url: &str,
    mcp_session_id: &str,
    id: u64,
    tool: &str,
    arguments: Value,
) -> Value {
    let (_, text) = mcp_post(
        client,
        base_url,
        Some(mcp_session_id),
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }),
    )
    .await;
    parse_mcp_tool_response(&text, id)
}

async fn mcp_post(
    client: &reqwest::Client,
    base_url: &str,
    mcp_session_id: Option<&str>,
    body: Value,
) -> (HeaderMap, String) {
    let mut req = client
        .post(format!("{base_url}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(sid) = mcp_session_id {
        req = req.header("MCP-Session-Id", sid);
    }
    let resp = req.send().await.expect("mcp post");
    let headers = resp.headers().clone();
    let text = resp.text().await.expect("mcp body");
    (headers, text)
}

fn mcp_session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("mcp-session-id")
        .or_else(|| headers.get("MCP-Session-Id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn parse_mcp_tool_response(text: &str, id: u64) -> Value {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(j) = serde_json::from_str::<Value>(rest) else {
            continue;
        };
        if j.get("id").and_then(|v| v.as_u64()) == Some(id) {
            if let Some(err) = j.get("error") {
                return json!({ "_run_error": err.to_string() });
            }
            let result = j.get("result").cloned().unwrap_or(json!({}));
            if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
                let msg = result
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("mcp tool error");
                return json!({ "_run_error": msg });
            }
            return normalize_mcp_tool_result(result);
        }
    }
    if let Ok(j) = serde_json::from_str::<Value>(text) {
        if j.get("id").and_then(|v| v.as_u64()) == Some(id) {
            return normalize_mcp_tool_result(j.get("result").cloned().unwrap_or(json!({})));
        }
    }
    panic!("no MCP JSON-RPC result for id={id} in: {text}");
}

fn normalize_mcp_tool_result(result: Value) -> Value {
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let meta = result.get("_meta").cloned().unwrap_or(json!({}));
    json!({
        "run_markdown": text,
        "_meta": meta,
        "mcp_result": result,
    })
}

pub fn plasm_meta(body: &Value) -> &Value {
    if let Some(plasm) = body.get("_meta").and_then(|m| m.get("plasm")) {
        return plasm;
    }
    body.get("_meta")
        .expect("missing _meta or _meta.plasm")
}

pub fn markdown_text(body: &Value) -> &str {
    body.get("run_markdown")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("mcp_result").and_then(|r| {
            r.get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
        }))
        .unwrap_or("")
}

pub fn continuity_phase(body: &Value) -> Option<&str> {
    plasm_meta(body)
        .get("continuity")
        .and_then(|c| c.get("phase"))
        .and_then(|v| v.as_str())
}

pub fn plan_commit_ref(body: &Value) -> Option<String> {
    plasm_meta(body)
        .get("plan_commit_ref")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn dry_verdict(body: &Value) -> Option<&str> {
    plasm_meta(body)
        .get("dry_verdict")
        .and_then(|v| v.as_str())
}

pub fn operation_handle_from_accept(body: &Value) -> String {
    plasm_meta(body)
        .get("continuity")
        .and_then(|c| c.get("operation_handle"))
        .and_then(|v| v.as_str())
        .expect("operation_handle in accept response")
        .to_string()
}

pub fn wait_program(handle: &str) -> String {
    format!("wait({handle})")
}

pub fn cancel_program(handle: &str) -> String {
    format!("cancel({handle})")
}

pub fn assert_async_accept(body: &Value, handle_prefix: &str) {
    let md = markdown_text(body);
    assert!(
        md.contains('`') && md.contains(handle_prefix),
        "expected async accept markdown with `{handle_prefix}`: {md}"
    );
    assert!(
        md.contains("wait(") || md.contains("Poll:"),
        "expected wait hint in markdown: {md}"
    );
    assert_eq!(
        continuity_phase(body),
        Some("running"),
        "expected continuity.phase=running: {body}"
    );
    let handle = plasm_meta(body)
        .get("continuity")
        .and_then(|c| c.get("operation_handle"))
        .and_then(|v| v.as_str())
        .expect("operation_handle");
    assert!(
        handle.starts_with(handle_prefix),
        "expected handle prefix {handle_prefix}, got {handle}"
    );
}

pub fn assert_running_wait(body: &Value) {
    let phase = continuity_phase(body);
    assert_eq!(phase, Some("running"), "expected running wait: {body}");
    let md = markdown_text(body);
    assert!(
        md.contains("running") || md.contains("step"),
        "expected running/progress markdown: {md}"
    );
}

pub fn assert_terminal_success(body: &Value) {
    if body.get("operation").and_then(|v| v.as_bool()) == Some(true) {
        let phase = continuity_phase(body);
        assert!(
            phase.is_none() || phase == Some("succeeded"),
            "unexpected operation phase on terminal: {body}"
        );
    }
    let has_rows = body.is_array()
        || body.get("results").is_some()
        || body
            .get("return_steps")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
    let md = markdown_text(body);
    assert!(
        has_rows || (!md.is_empty() && !md.contains("cancelled") && continuity_phase(body) != Some("running")),
        "expected terminal success body: {body}"
    );
}

pub fn assert_cancelled(body: &Value) {
    assert_eq!(
        continuity_phase(body),
        Some("cancelled"),
        "expected cancelled continuity: {body}"
    );
    let md = markdown_text(body);
    assert!(md.contains("cancelled"), "expected cancelled markdown: {md}");
}

pub fn assert_review_gate_error(err: &str) {
    assert!(
        err.contains("plan_requires_review"),
        "expected plan_requires_review error, got: {err}"
    );
}