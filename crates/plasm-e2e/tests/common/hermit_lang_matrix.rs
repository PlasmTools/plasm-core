//! Hermit instance for `plasm_language_matrix` OpenAPI (compiled only by `plasm_language_matrix` integration test).
//!
//! LangCursor routes are served by an in-memory sidecar so PLP-8 iterate-until can re-observe
//! advancing `phase` after each `tick` (Hermit schema examples are otherwise immutable).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::OnceCell;

static LANGUAGE_MATRIX_HERMIT: OnceCell<String> = OnceCell::const_new();
/// Dedicated host for [`plasm_language_matrix_live_runs`] so focused live tests cannot poison
/// Hermit's mutable example store before early matrix rows (e.g. `lang_relation_lines`).
static LANGUAGE_MATRIX_SUITE_HERMIT: OnceCell<String> = OnceCell::const_new();

#[derive(Clone)]
struct CursorLab {
    store: Arc<Mutex<HashMap<String, CursorRow>>>,
}

#[derive(Clone, Debug)]
struct CursorRow {
    id: String,
    phase: &'static str,
    ticks: u32,
    /// When true, ticks never reach `done` (bound-exhaustion witness).
    stuck: bool,
}

impl CursorRow {
    fn bootstrap(id: &str) -> Self {
        let stuck = id == "c_stuck" || id.starts_with("stuck_");
        let phase = if id == "c_done" || id.starts_with("done_") {
            "done"
        } else {
            "open"
        };
        Self {
            id: id.to_string(),
            phase,
            ticks: 0,
            stuck,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "phase": self.phase,
            "ticks": self.ticks,
        })
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);
        if self.stuck {
            return;
        }
        self.phase = match self.phase {
            "open" => "mid",
            "mid" => "done",
            other => other,
        };
    }
}

fn language_matrix_spec_path() -> std::path::PathBuf {
    let crate_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        crate_root.join("../../fixtures/real_openapi_specs/plasm_language_matrix.yaml"),
        crate_root.join("fixtures/real_openapi_specs/plasm_language_matrix.yaml"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    panic!(
        "Cannot find plasm_language_matrix.yaml (tried {:?})",
        candidates
    );
}

async fn get_cursor(
    State(lab): State<CursorLab>,
    Path(id): Path<String>,
) -> Json<Value> {
    let mut guard = lab.store.lock().expect("cursor lab lock");
    let row = guard
        .entry(id.clone())
        .or_insert_with(|| CursorRow::bootstrap(&id));
    Json(row.to_json())
}

async fn tick_cursor(
    State(lab): State<CursorLab>,
    Path(id): Path<String>,
) -> Json<Value> {
    let mut guard = lab.store.lock().expect("cursor lab lock");
    let row = guard
        .entry(id.clone())
        .or_insert_with(|| CursorRow::bootstrap(&id));
    row.tick();
    Json(row.to_json())
}

async fn reset_cursors(State(lab): State<CursorLab>) -> StatusCode {
    lab.store.lock().expect("cursor lab lock").clear();
    StatusCode::NO_CONTENT
}

fn cursor_lab_router(lab: CursorLab) -> Router {
    Router::new()
        .route("/language/v1/cursors/_lab_reset", post(reset_cursors))
        .route("/language/v1/cursors/{id}", get(get_cursor))
        .route("/language/v1/cursors/{id}/tick", post(tick_cursor))
        .with_state(lab)
}

async fn spawn_hermit_host_root(spec_path: &std::path::Path) -> String {
    // Hermit's schema faker may emit sparse objects; `for_each` templates read row JSON (`_.id`).
    // Prefer declared OpenAPI `example` payloads so list GETs return stable primary keys (i1, i2, …).
    beavuck_hermit::resource_generator::set_use_examples(true);
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let hermit_router = beavuck_hermit::router::build_with_bounds(routes, 1, 5);
    let lab = CursorLab {
        store: Arc::new(Mutex::new(HashMap::new())),
    };
    // Stateful cursor routes take precedence; unmatched paths fall through to Hermit.
    let router = cursor_lab_router(lab).fallback_service(hermit_router);

    // Bind the listener on the *server* runtime. Creating it on the caller's runtime and then
    // dropping that runtime (e.g. views `block_on_views_live` harness) orphans the IO driver and
    // kills accept — every later test then sees connection-refused on the cached OnceCell URL.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .expect("hermit server runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("hermit bind");
            let addr = listener.local_addr().expect("hermit local_addr");
            let base_url = format!("http://127.0.0.1:{}", addr.port());
            tx.send(base_url).expect("send hermit base url");
            axum::serve(listener, router).await.expect("hermit serve");
        });
    });

    let base_url = rx.recv().expect("hermit base url");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    base_url
}

/// Paths `/language/v1/...` from host root (shared by focused live / views helpers).
pub async fn language_matrix_hermit_base_url() -> &'static String {
    hermit_base_url(&LANGUAGE_MATRIX_HERMIT).await
}

/// Isolated Hermit for the full matrix live harness (must not share mutable state with
/// `lang_*_live` focused tests that create/patch against the shared host).
pub async fn language_matrix_suite_hermit_base_url() -> &'static String {
    hermit_base_url(&LANGUAGE_MATRIX_SUITE_HERMIT).await
}

async fn hermit_base_url(cell: &'static OnceCell<String>) -> &'static String {
    cell.get_or_init(|| async {
        let spec_path = language_matrix_spec_path();
        spawn_hermit_host_root(spec_path.as_path()).await
    })
    .await
}

/// Clear LangCursor in-memory state on a Hermit base URL so iterate-until rows
/// start from bootstrap phases.
pub async fn language_matrix_reset_lang_cursors_on(base: &str) {
    let _ = reqwest::Client::new()
        .post(format!("{base}/language/v1/cursors/_lab_reset"))
        .send()
        .await;
}
