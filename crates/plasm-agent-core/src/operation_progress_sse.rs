//! Shared SSE stream helper for operation progress (wire line or JSON payloads).

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream, StreamExt};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

use crate::op_ui_telemetry::OpUiTelemetry;
use crate::operation_progress::OpProgressEvent;

type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;
type PollFuture = Pin<Box<dyn Future<Output = Option<OpUiTelemetry>> + Send>>;

fn progress_events_from_broadcast<F>(
    rx: broadcast::Receiver<OpProgressEvent>,
    last_seq: u64,
    event_data: F,
) -> SseBody
where
    F: Fn(&OpProgressEvent) -> (String, &'static str) + Send + Sync + 'static,
{
    let event_data = Arc::new(event_data);
    Box::pin(stream::unfold(
        (rx, last_seq),
        move |(mut rx, mut last_seq)| {
            let event_data = event_data.clone();
            async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            if ev.seq <= last_seq {
                                continue;
                            }
                            last_seq = ev.seq;
                            let (data, event_name) = event_data(&ev);
                            return Some((
                                Ok(Event::default().event(event_name).data(data)),
                                (rx, last_seq),
                            ));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        },
    ))
}

fn json_event_name(last_seq: u64, terminal: bool) -> &'static str {
    if terminal {
        "terminal"
    } else if last_seq == 0 {
        "snapshot"
    } else {
        "progress"
    }
}

/// Plain-text wire-line SSE (`GET /execute/.../operations/{handle}/stream`).
pub fn operation_progress_wire_sse(
    rx: broadcast::Receiver<OpProgressEvent>,
    initial_seq: u64,
    initial_line: String,
) -> Response {
    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("snapshot").data(initial_line))
    });
    let body = first.chain(progress_events_from_broadcast(rx, initial_seq, |ev| {
        let name = if ev.terminal { "terminal" } else { "progress" };
        (ev.line.clone(), name)
    }));
    Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// JSON payload SSE from in-memory broadcast (`GET /v1/run/ui/progress/.../stream`).
pub fn operation_progress_json_sse(
    rx: broadcast::Receiver<OpProgressEvent>,
    initial_seq: u64,
    initial_json: String,
) -> Response {
    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("snapshot").data(initial_json))
    });
    let body = first.chain(progress_events_from_broadcast(rx, initial_seq, |ev| {
        let json = OpUiTelemetry::from_progress_event(ev).json_line();
        let name = if ev.terminal { "terminal" } else { "progress" };
        (json, name)
    }));
    Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// JSON payload SSE via periodic poll (cross-replica / rehydrated stub path).
pub fn operation_progress_json_poll_sse<St>(
    interval: Duration,
    state: St,
    poll: Arc<dyn Fn(St) -> PollFuture + Send + Sync>,
) -> Response
where
    St: Clone + Send + Sync + 'static,
{
    let body = stream::unfold((0_u64, false, state), move |(last_seq, finished, st)| {
        let poll = poll.clone();
        async move {
            if finished {
                return None;
            }
            tokio::time::sleep(interval).await;
            let snap = poll(st.clone()).await?;
            if snap.n <= last_seq && !snap.terminal {
                return Some((
                    Ok::<Event, Infallible>(Event::default().comment("keepalive")),
                    (last_seq, false, st),
                ));
            }
            let event = json_event_name(last_seq, snap.terminal);
            Some((
                Ok(Event::default().event(event).data(snap.json_line())),
                (snap.n, snap.terminal, st),
            ))
        }
    });
    Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response()
}
