//! E2E push coverage: HTTP operation SSE + MCP `notifications/plasm/op`.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
#[allow(dead_code)]
mod language_matrix;

#[path = "common/long_operation.rs"]
#[allow(dead_code)]
mod long_operation;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::response::Response;
use axum::Router;

use long_operation::{
    assert_async_accept, assert_mcp_op_notification_params, assert_plain_op_wire_line,
    http_collect_operation_sse_events, mcp_notification_from_sse_data,
    operation_handle_from_accept, parse_sse_events, LongOpFixture, RunOpts, Surface,
};
const SLOW_LANG_ITEM: &str = "class ReadPagedItems(Program):\n    def build(self):\n        return e1.query().page_size(1).take(10)\n";
const COUNT_ALL_LANG_ITEMS: &str = "class CountItems(Program):\n    def build(self):\n        return e1.query().aggregate(n=agg.count())\n";
use reqwest::StatusCode;
use tokio::sync::{mpsc, oneshot, Semaphore};

#[derive(Clone)]
struct GatedBackend {
    upstream: String,
    client: reqwest::Client,
    first_request: Arc<AtomicBool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

async fn forward_after_gate(State(backend): State<GatedBackend>, request: Request) -> Response {
    if !backend.first_request.swap(true, Ordering::SeqCst) {
        backend.entered.add_permits(1);
        backend
            .release
            .acquire()
            .await
            .expect("backend gate open")
            .forget();
    }
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .expect("fixture request body");
    let response = backend
        .client
        .request(parts.method, format!("{}{}", backend.upstream, parts.uri))
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .expect("forward fixture request to Hermit");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.expect("Hermit response body");
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

async fn gated_mcp_fixture() -> (LongOpFixture, GatedBackend, tokio::task::JoinHandle<()>) {
    let backend = GatedBackend {
        upstream: hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone(),
        client: reqwest::Client::new(),
        first_request: Arc::new(AtomicBool::new(false)),
        entered: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gated backend");
    let base_url = format!("http://{}", listener.local_addr().expect("backend address"));
    let app = Router::new()
        .fallback(forward_after_gate)
        .with_state(backend.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve gated backend");
    });
    let fixture = Box::pin(LongOpFixture::setup_with_backend(base_url)).await;
    (fixture, backend, task)
}

async fn spawn_mcp_op_notification_listener(
    client: reqwest::Client,
    base_url: String,
    mcp_session_id: String,
) -> mpsc::Receiver<serde_json::Value> {
    let (tx, rx) = mpsc::channel(32);
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(async move {
        let resp = client
            .get(format!("{base_url}/mcp"))
            .header("MCP-Session-Id", &mcp_session_id)
            .header("accept", "text/event-stream")
            .send()
            .await
            .expect("open MCP notification stream");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "MCP notification stream status"
        );
        ready_tx.send(()).expect("listener readiness receiver");
        let mut buf = String::new();
        let mut resp = resp;
        loop {
            match tokio::time::timeout(Duration::from_millis(800), resp.chunk()).await {
                Ok(Ok(Some(chunk))) => {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    for ev in parse_sse_events(&buf) {
                        if let Some(params) = mcp_notification_from_sse_data(&ev.data) {
                            let _ = tx.send(params).await;
                        }
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
    });
    ready_rx.await.expect("MCP notification listener ready");
    rx
}

#[test]
fn operation_progress_push_http_sse_and_mcp_notifications() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(operation_progress_push_http_sse_and_mcp_notifications_async());
        })
        .expect("spawn operation_progress_push e2e thread")
        .join()
        .expect("join");
}

async fn operation_progress_push_http_sse_and_mcp_notifications_async() {
    let fixture = LongOpFixture::setup().await;

    let handle = {
        let body = fixture
            .run_program(
                Surface::Http,
                SLOW_LANG_ITEM,
                RunOpts {
                    wait: false,
                    force: true,
                    ..Default::default()
                },
            )
            .await
            .expect("async accept");
        assert_async_accept(&body, Surface::Http.async_handle_prefix());
        operation_handle_from_accept(&body)
    };

    let events = http_collect_operation_sse_events(
        &fixture.client,
        &fixture.base_url,
        &fixture.http_prompt_hash,
        &fixture.http_session_id,
        &handle,
        Duration::from_secs(8),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|e| e.event.as_deref() == Some("snapshot")),
        "expected snapshot event, got: {events:?}"
    );
    for ev in &events {
        assert_plain_op_wire_line(&ev.data);
    }
    assert!(
        events
            .iter()
            .any(|e| { e.event.as_deref() == Some("terminal") || e.data.contains('!') }),
        "expected terminal progress line, got: {events:?}"
    );
    fixture.cleanup().await;

    let (fixture, backend, backend_task) = Box::pin(gated_mcp_fixture()).await;
    let mut notify_rx = spawn_mcp_op_notification_listener(
        fixture.client.clone(),
        fixture.base_url.clone(),
        fixture.mcp_transport_id.clone(),
    )
    .await;

    // Aggregate requires complete collection coverage, unlike the bounded HTTP read.
    let plan = fixture.plan_dry(Surface::Mcp, COUNT_ALL_LANG_ITEMS).await;
    let receipt = plan
        .pointer("/mcp_result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("MCP plan content receipt");
    let run_ref = receipt
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('\t')?;
            (name == "run_ref").then_some(value.to_string())
        })
        .expect("reviewed MCP run_ref");
    assert!(
        receipt.lines().any(|line| line == "dry_verdict\treview"),
        "full-collection aggregate must require review: {receipt}"
    );
    let run = fixture.run_program(
        Surface::Mcp,
        COUNT_ALL_LANG_ITEMS,
        RunOpts {
            run_ref: Some(run_ref),
            ..Default::default()
        },
    );
    tokio::pin!(run);
    tokio::select! {
        biased;
        completed = &mut run => {
            panic!("MCP run completed without reaching its backend gate: {completed:?}");
        }
        entered = tokio::time::timeout(Duration::from_secs(10), backend.entered.acquire()) => {
            entered
                .expect("MCP backend request before deadline")
                .expect("backend entered gate")
                .forget();
        }
    }
    // Current MCP execution awaits terminal inline; push must arrive while it runs,
    // rather than relying on retired MCP async accepts or wait-program polling.
    let first = tokio::select! {
        // If both are ready, completion wins: a queued notification must not
        // disguise a run that has already completed.
        biased;
        completed = &mut run => {
            panic!("MCP run completed before its first operation push: {completed:?}");
        }
        notification = tokio::time::timeout(Duration::from_secs(10), notify_rx.recv()) => {
            notification
                .expect("operation push before deadline")
                .expect("notification stream open")
        }
    };
    let mut notifications = vec![first];
    assert_mcp_op_notification_params(&notifications[0]);
    backend.release.add_permits(1);
    while let Ok(notification) = notify_rx.try_recv() {
        notifications.push(notification);
    }
    let body = run.await.expect("MCP reviewed live run");
    assert_ne!(
        body.pointer("/mcp_result/isError")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let run_markdown = body
        .pointer("/mcp_result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("terminal MCP result Markdown");
    let rows = run_markdown
        .split_once("```tsv\n")
        .and_then(|(_, table)| table.split_once("```"))
        .map(|(table, _)| table)
        .expect("terminal aggregate must inline TSV rows");
    let mut rows = rows.lines();
    assert_eq!(
        rows.next(),
        Some("n"),
        "aggregate count column: {run_markdown}"
    );
    let count = rows
        .next()
        .expect("aggregate count row")
        .parse::<u64>()
        .expect("aggregate count must be an unsigned integer");
    assert!(count > 0, "fixture aggregate must count actual rows");
    assert!(
        rows.next().is_none(),
        "aggregate must return exactly one row"
    );
    let steps = body
        .pointer("/_meta/plasm/steps")
        .and_then(serde_json::Value::as_array)
        .expect("terminal MCP run steps");
    assert!(!steps.is_empty(), "terminal run must retain step evidence");
    for step in steps {
        let run_id = step
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .expect("step run_id");
        let artifact_uri = step
            .get("artifact_uri")
            .and_then(serde_json::Value::as_str)
            .expect("step artifact URI");
        assert!(!run_id.is_empty(), "step must identify its stored run");
        assert!(
            artifact_uri.starts_with("plasm://") && artifact_uri.ends_with(run_id),
            "step must link its run artifact: {step}"
        );
    }

    assert!(
        !notifications.is_empty(),
        "expected at least one notifications/plasm/op during slow run"
    );
    for params in &notifications {
        assert_mcp_op_notification_params(params);
    }
    fixture.cleanup().await;
    backend_task.abort();
}
