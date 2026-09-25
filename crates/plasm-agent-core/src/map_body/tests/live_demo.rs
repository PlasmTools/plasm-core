//! Manual local viewer for actual mock-backed runtime events, never a simulated event stream.
use super::*;
use crate::operation::ExecutionScope;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
type ActiveOperation = (Arc<ExecuteSession>, plasm_core::OperationHandle);
#[derive(Clone, Default)]
struct Demo(Arc<Mutex<Option<ActiveOperation>>>);

async fn start(State(state): State<Demo>) -> Json<Value> {
    if let Some((es, handle)) = state.0.lock().unwrap().as_ref() {
        es.cancel_operation(handle, None);
    }
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let (es, host, _) = fixture_with_controls(3, None, Some(gate.clone()));
    let es = Arc::new(es);
    let (root, body) = program(&es);
    let bundle = compose(root, body).unwrap();
    let comp = bundle.artifact().comp.clone();
    let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
    let cancel = plasm_runtime::CancelSignal::new();
    es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
        .unwrap();
    *state.0.lock().unwrap() = Some((es.clone(), handle.clone()));
    tokio::spawn(async move {
        let release = tokio::spawn(async move {
            for _ in 0..3 {
                gate.0.notified().await;
                tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                gate.1.notify_one();
            }
        });
        let scope = ExecutionScope::for_async_operation(es.clone(), handle.clone(), cancel);
        let result = Box::pin(crate::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "live-demo",
            &bundle,
            true,
            None,
            Some(&scope),
            None,
            None,
        ))
        .await;
        release.abort();
        match result {
            Ok(run) => es.finalize_operation_succeeded(&handle, run, None),
            Err(error) => es.finalize_operation_failed(&handle, error, None),
        }
    });
    Json(json!({"comp":comp}))
}
async fn stream(State(state): State<Demo>) -> axum::response::Response {
    let Some((es, handle)) = state.0.lock().unwrap().clone() else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let rx = es.operation_progress_subscribe(&handle).unwrap();
    let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
    crate::operation_progress_sse::operation_progress_json_sse(
        rx,
        snapshot.n,
        snapshot.json_line(),
        Arc::new(move || crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle)),
    )
}
async fn cancel(State(state): State<Demo>) -> Json<Value> {
    if let Some((es, handle)) = state.0.lock().unwrap().as_ref() {
        es.cancel_operation(handle, None);
    }
    Json(json!({"ok":true}))
}
#[test]
#[ignore = "manual local mock-backed runtime viewer on 127.0.0.1:8768"]
fn python_host_live_demo() {
    on_runtime(async {
        let html = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../docs/research/python-dag-slice/live-runtime-preview.html"),
        )
        .unwrap();
        let app = Router::new()
            .route(
                "/",
                get(move || {
                    let html = html.clone();
                    async move { Html(html) }
                }),
            )
            .route("/start", post(start))
            .route("/stream", get(stream))
            .route("/cancel", post(cancel))
            .with_state(Demo::default());
        let socket = tokio::net::TcpListener::bind("127.0.0.1:8768")
            .await
            .unwrap();
        eprintln!("Live mock runtime viewer: http://127.0.0.1:8768/");
        axum::serve(socket, app).await.unwrap();
    });
}
