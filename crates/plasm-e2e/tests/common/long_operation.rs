//! Hermit + unified HTTP/MCP server harness for long-operation E2E tests.

#![allow(clippy::type_complexity)]
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
/// Bare list queries are host-page-bounded (`ok`); aggregate still requires plan review.
pub const REVIEW_GATE_LANG_ITEM: &str = "LangItem.aggregate(n=count)";
pub const UNBOUNDED_LANG_ITEM: &str = REVIEW_GATE_LANG_ITEM;
pub const BOUNDED_LANG_ITEM: &str = "LangItem.limit(2)";
/// Paginated list — slow enough to observe Running/cancel without unbounded review fanout.
pub const SLOW_LANG_ITEM: &str = "LangItem.page_size(1).limit(10)";

async fn spawn_unified_server() -> (String, JoinHandle<()>) {
    let hermit = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
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

static SHARED_SERVER: OnceCell<tokio::sync::Mutex<Option<(String, JoinHandle<()>)>>> =
    OnceCell::const_new();

async fn shared_server_base_url() -> String {
    let lock = SHARED_SERVER
        .get_or_init(|| async { tokio::sync::Mutex::new(None) })
        .await;
    let mut guard = lock.lock().await;
    let needs_spawn = match guard.as_ref() {
        None => true,
        Some((_, handle)) => handle.is_finished(),
    };
    if needs_spawn {
        *guard = Some(spawn_unified_server().await);
    }
    guard.as_ref().expect("shared server slot").0.clone()
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
    pub run_ref: Option<String>,
    pub plan_only: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            wait: true,
            force: false,
            run_ref: None,
            plan_only: false,
        }
    }
}

pub struct LongOpFixture {
    pub base_url: String,
    pub client: reqwest::Client,
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
        let (http_prompt_hash, http_session_id) = http_open_session(&client, &base_url)
            .await
            .expect("http open session");
        let mcp_transport_id = mcp_initialize(&client, &base_url).await;
        let logical_session_ref = mcp_plasm_context(&client, &base_url, &mcp_transport_id).await;
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
            Surface::Http => http_post_program(
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
            .expect("http plan dry"),
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
                if program.starts_with("wait(") || program.starts_with("cancel(") {
                    return Ok(mcp_tool_call(
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
                    .await)
                    .and_then(|body| {
                        if let Some(err) = body.get("_run_error").and_then(|v| v.as_str()) {
                            Err(err.to_string())
                        } else {
                            Ok(body)
                        }
                    });
                }
                let run_ref = if let Some(pc) = opts.run_ref {
                    pc
                } else if opts.force {
                    let dry = mcp_tool_call(
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
                    if let Some(err) = dry.get("_run_error").and_then(|v| v.as_str()) {
                        return Err(err.to_string());
                    }
                    run_ref_from_meta(&dry)
                        .ok_or_else(|| "missing run_ref from MCP plasm dry-run".to_string())?
                } else {
                    return Ok(mcp_tool_call(
                        &self.client,
                        &self.base_url,
                        &self.mcp_transport_id,
                        self.next_rpc_id(),
                        "plasm_run",
                        json!({
                            "logical_session_ref": self.logical_session_ref,
                        }),
                    )
                    .await)
                    .and_then(|body| {
                        if let Some(err) = body.get("_run_error").and_then(|v| v.as_str()) {
                            Err(err.to_string())
                        } else {
                            Ok(body)
                        }
                    });
                };
                Ok(mcp_tool_call(
                    &self.client,
                    &self.base_url,
                    &self.mcp_transport_id,
                    self.next_rpc_id(),
                    "plasm_run",
                    json!({
                        "logical_session_ref": self.logical_session_ref,
                        "run_ref": run_ref,
                    }),
                )
                .await)
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
        let _ = self
            .run_program(Surface::Http, "cancel(o1)", RunOpts::default())
            .await;
        let mcp_cancel = format!("cancel({}_o1)", self.logical_session_ref);
        let _ = self
            .run_program(Surface::Mcp, &mcp_cancel, RunOpts::default())
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

impl Surface {
    pub fn async_handle_prefix(self) -> &'static str {
        match self {
            Surface::Http => "o",
            Surface::Mcp => "l_",
        }
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
    if let Some(ref pc) = opts.run_ref {
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
        .expect("logical_session_ref from plasm_context")
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
    body.get("_meta").expect("missing _meta or _meta.plasm")
}

pub fn markdown_text(body: &Value) -> &str {
    body.get("run_markdown")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("mcp_result").and_then(|r| {
                r.get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
            })
        })
        .unwrap_or("")
}

pub fn continuity_phase(body: &Value) -> Option<&str> {
    plasm_meta(body)
        .get("continuity")
        .and_then(|c| c.get("p").or_else(|| c.get("phase")))
        .and_then(|v| v.as_str())
}

fn extract_operation_handle_from_markdown(md: &str) -> Option<String> {
    for part in md.split('`') {
        if part.contains("_o") && part.starts_with("l_") {
            return Some(part.to_string());
        }
        if part.len() > 1 && part.starts_with('o') && part[1..].chars().all(|c| c.is_ascii_digit())
        {
            return Some(part.to_string());
        }
    }
    None
}

fn continuity_handle(body: &Value) -> Option<&str> {
    plasm_meta(body)
        .get("continuity")
        .and_then(|c| c.get("h").or_else(|| c.get("operation_handle")))
        .and_then(|v| v.as_str())
}

pub fn operation_handle_from_accept(body: &Value) -> String {
    if let Some(h) = continuity_handle(body) {
        if h.contains("_o")
            || (h.len() > 1 && h.starts_with('o') && h[1..].chars().all(|c| c.is_ascii_digit()))
        {
            return h.to_string();
        }
    }
    extract_operation_handle_from_markdown(markdown_text(body))
        .expect("operation handle in accept response")
}

pub fn run_ref_from_meta(body: &Value) -> Option<String> {
    plasm_meta(body)
        .get("run_ref")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn dry_verdict(body: &Value) -> Option<&str> {
    plasm_meta(body).get("dry_verdict").and_then(|v| v.as_str())
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
        md.contains('+') || md.contains("wait("),
        "expected compact accept line: {md}"
    );
    assert!(
        !md.contains("Poll:"),
        "accept must not repeat poll instructions: {md}"
    );
    assert_eq!(
        continuity_phase(body),
        Some("running"),
        "expected continuity.phase=running: {body}"
    );
    let handle = continuity_handle(body).expect("operation handle");
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
        md.contains('~') || md.contains('='),
        "expected compact running/unchanged markdown (~ or =): {md}"
    );
    assert!(
        !md.contains("Poll:"),
        "running poll must not repeat instructions: {md}"
    );
    if let Some(op) = plasm_meta(body).get("op") {
        assert!(op.get("n").is_some(), "short-key op.n required: {op}");
        if md.contains('=') {
            assert!(
                op.get("=").is_some(),
                "unchanged poll should set op.= : {op}"
            );
        }
    } else if md.contains('~') {
        panic!("running poll with ~ should include _meta.plasm.op: {body}");
    }
}

/// One SSE frame from `text/event-stream` (operation progress or MCP).
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

pub fn parse_sse_events(text: &str) -> Vec<SseEvent> {
    let mut out = Vec::new();
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            if !data_lines.is_empty() {
                out.push(SseEvent {
                    event: event_name.take(),
                    data: data_lines.join("\n"),
                });
                data_lines.clear();
            }
            continue;
        }
        if let Some(ev) = line.strip_prefix("event: ") {
            event_name = Some(ev.to_string());
        } else if let Some(d) = line.strip_prefix("data: ") {
            data_lines.push(d.to_string());
        }
    }
    if !data_lines.is_empty() {
        out.push(SseEvent {
            event: event_name,
            data: data_lines.join("\n"),
        });
    }
    out
}

pub fn assert_plain_op_wire_line(data: &str) {
    assert!(
        data.contains('`'),
        "expected backtick wire line, got: {data}"
    );
    assert!(
        !data.trim_start().starts_with('{'),
        "progress SSE data must be plain line, not JSON: {data}"
    );
}

/// Collect operation-progress SSE until `terminal` or timeout.
pub async fn http_collect_operation_sse_events(
    client: &reqwest::Client,
    base_url: &str,
    prompt_hash: &str,
    session_id: &str,
    handle: &str,
    timeout: Duration,
) -> Vec<SseEvent> {
    let url = format!("{base_url}/execute/{prompt_hash}/{session_id}/operations/{handle}/stream");
    let resp = client
        .get(&url)
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("operation SSE GET");
    assert_eq!(resp.status(), StatusCode::OK, "operation SSE status");
    let mut buf = String::new();
    let deadline = std::time::Instant::now() + timeout;
    let mut resp = resp;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                let events = parse_sse_events(&buf);
                if events
                    .iter()
                    .any(|e| e.event.as_deref() == Some("terminal"))
                {
                    return events;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => panic!("operation SSE chunk error: {e}"),
            Err(_) => continue,
        }
    }
    parse_sse_events(&buf)
}

pub fn mcp_notification_from_sse_data(data: &str) -> Option<Value> {
    let j: Value = serde_json::from_str(data).ok()?;
    if j.get("method").and_then(|m| m.as_str()) != Some("notifications/plasm/op") {
        return None;
    }
    j.get("params").cloned()
}

/// Open MCP GET stream and collect `notifications/plasm/op` until `deadline`.
pub async fn mcp_collect_op_notifications(
    client: &reqwest::Client,
    base_url: &str,
    mcp_session_id: &str,
    deadline: Duration,
) -> Vec<Value> {
    let resp = client
        .get(format!("{base_url}/mcp"))
        .header("MCP-Session-Id", mcp_session_id)
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("mcp GET stream");
    assert_eq!(resp.status(), StatusCode::OK, "mcp GET stream status");
    let mut buf = String::new();
    let mut out = Vec::new();
    let until = std::time::Instant::now() + deadline;
    let mut resp = resp;
    while std::time::Instant::now() < until {
        match tokio::time::timeout(Duration::from_millis(500), resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                for ev in parse_sse_events(&buf) {
                    if let Some(params) = mcp_notification_from_sse_data(&ev.data) {
                        out.push(params);
                    }
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => panic!("mcp SSE chunk error: {e}"),
            Err(_) => continue,
        }
    }
    out
}

pub fn assert_mcp_op_notification_params(params: &Value) {
    assert!(
        params.get("line").and_then(|v| v.as_str()).is_some(),
        "notification params.line required: {params}"
    );
    assert!(
        params.get("n").is_some(),
        "notification params.n required: {params}"
    );
    let extra: Vec<_> = params
        .as_object()
        .map(|m| {
            m.keys()
                .filter(|k| *k != "line" && *k != "n" && *k != "c")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    assert!(
        extra.is_empty(),
        "notification params should be line+n (+ optional c): {params}"
    );
    assert_plain_op_wire_line(params.get("line").and_then(|v| v.as_str()).unwrap());
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
        has_rows
            || (!md.is_empty()
                && !md.contains("cancelled")
                && continuity_phase(body) != Some("running")),
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
    assert!(
        md.contains('x') || md.contains("cancelled"),
        "expected cancelled markdown: {md}"
    );
}

pub fn assert_review_gate_error(err: &str) {
    assert!(
        err.contains("plan_requires_review")
            || err.contains("run_ref")
            || err.contains("call `plasm` first"),
        "expected plan review gate error, got: {err}"
    );
}
