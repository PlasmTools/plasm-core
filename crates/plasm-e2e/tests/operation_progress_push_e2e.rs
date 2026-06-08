//! E2E push coverage: HTTP operation SSE + MCP `notifications/plasm/op`.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
#[allow(dead_code)]
mod language_matrix;

#[path = "common/long_operation.rs"]
#[allow(dead_code)]
mod long_operation;

use std::time::Duration;

use long_operation::{
    assert_async_accept, assert_mcp_op_notification_params, assert_plain_op_wire_line,
    http_collect_operation_sse_events, mcp_notification_from_sse_data,
    operation_handle_from_accept, parse_sse_events, wait_program, LongOpFixture, RunOpts, Surface,
    SLOW_LANG_ITEM,
};
use reqwest::StatusCode;
use tokio::sync::mpsc;

async fn spawn_mcp_op_notification_listener(
    client: reqwest::Client,
    base_url: String,
    mcp_session_id: String,
) -> mpsc::Receiver<serde_json::Value> {
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let Ok(resp) = client
            .get(format!("{base_url}/mcp"))
            .header("MCP-Session-Id", &mcp_session_id)
            .header("accept", "text/event-stream")
            .send()
            .await
        else {
            return;
        };
        if resp.status() != StatusCode::OK {
            return;
        }
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
        assert_async_accept(&body, "s0_o");
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

    let mut notify_rx = spawn_mcp_op_notification_listener(
        fixture.client.clone(),
        fixture.base_url.clone(),
        fixture.mcp_transport_id.clone(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let body = fixture
        .run_program(
            Surface::Mcp,
            SLOW_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await
        .expect("mcp async accept");
    assert_async_accept(&body, &fixture.logical_session_ref);
    let handle = operation_handle_from_accept(&body);

    let mut notifications = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while notifications.is_empty() && std::time::Instant::now() < deadline {
        while let Ok(n) = notify_rx.try_recv() {
            notifications.push(n);
        }
        if !notifications.is_empty() {
            break;
        }
        let _ = fixture
            .run_program(Surface::Mcp, &wait_program(&handle), RunOpts::default())
            .await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }

    assert!(
        !notifications.is_empty(),
        "expected at least one notifications/plasm/op during slow run"
    );
    for params in &notifications {
        assert_mcp_op_notification_params(params);
    }
    fixture.cleanup().await;
}
