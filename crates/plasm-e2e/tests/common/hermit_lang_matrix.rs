//! Hermit instance for `plasm_language_matrix` OpenAPI (compiled only by `plasm_language_matrix` integration test).
//!
//! LangCursor routes are served by an in-memory sidecar so PLP-8 iterate-until can re-observe
//! advancing `phase` after each `tick` (Hermit schema examples are otherwise immutable).

#[path = "hermit_lang_matrix_items.rs"]
mod parity_items;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::OnceCell;

static LANGUAGE_MATRIX_HERMIT: OnceCell<String> = OnceCell::const_new();
/// Dedicated host for [`plasm_language_matrix_live_runs`] so focused live tests cannot poison
/// Hermit's mutable example store before early matrix rows (e.g. `lang_relation_lines`).
#[allow(dead_code)]
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

async fn get_cursor(State(lab): State<CursorLab>, Path(id): Path<String>) -> Json<Value> {
    let mut guard = lab.store.lock().expect("cursor lab lock");
    let row = guard
        .entry(id.clone())
        .or_insert_with(|| CursorRow::bootstrap(&id));
    Json(row.to_json())
}

async fn tick_cursor(State(lab): State<CursorLab>, Path(id): Path<String>) -> Json<Value> {
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

fn language_matrix_sidecar(lab: CursorLab) -> Router {
    Router::new()
        .route("/language/v1/cursors/_lab_reset", post(reset_cursors))
        .route("/language/v1/cursors/{id}", get(get_cursor))
        .route("/language/v1/cursors/{id}/tick", post(tick_cursor))
        .route("/language/v1/vaults", get(list_vaults))
        .route("/language/v1/vaults/{id}", get(get_vault))
        .route("/language/v1/vaults/{id}/unlock", post(unlock_vault))
        .route("/language/v1/tags", get(list_tags))
        .route("/language/v1/tags/{id}", get(get_tag))
        .route("/language/v1/lanes", get(list_lanes))
        .route("/language/v1/lane_stocks", get(list_lane_stocks))
        .with_state(lab)
}

// Relation reads must not grow the fixture collection. Hermit seeds unknown GET
// identities into its CRUD collection; hydration of embedded tag references could
// therefore change a later scoped query between the two source frontends.
#[derive(Debug, Deserialize)]
struct TagQuery {
    item_id: Option<String>,
    seq: Option<i64>,
}

fn tag_row(id: &str, item_id: &str, label: &str) -> Value {
    json!({ "id": id, "item_id": item_id, "label": label })
}

async fn list_tags(Query(query): Query<TagQuery>) -> Json<Value> {
    let parent = query
        .item_id
        .unwrap_or_else(|| format!("score-{}", query.seq.unwrap_or(0)));
    Json(json!([
        tag_row(&format!("{parent}::tag-urgent"), &parent, "urgent"),
        tag_row(&format!("{parent}::tag-review"), &parent, "review"),
    ]))
}

async fn get_tag(Path(id): Path<String>) -> Json<Value> {
    let (parent, label) = id.rsplit_once("::tag-").unwrap_or(("i1", "urgent"));
    Json(tag_row(&id, parent, label))
}

#[derive(Debug, Deserialize)]
struct ShelfQuery {
    shelf: Option<String>,
}

fn vault_row(id: &str, service: &str, password: &str) -> Value {
    json!({ "id": id, "service": service, "password": password })
}

fn lane_row(id: &str, title: &str, shelf: &str) -> Value {
    json!({ "id": id, "title": title, "shelf": shelf })
}

async fn get_vault(Path(id): Path<String>) -> Json<Value> {
    Json(match id.as_str() {
        "venmo" => vault_row("venmo", "venmo", "venmo-secret"),
        "paypal" => vault_row("paypal", "paypal", "paypal-secret"),
        _ => json!([]),
    })
}

async fn list_vaults() -> Json<Value> {
    Json(json!([
        vault_row("venmo", "venmo", "venmo-secret"),
        vault_row("paypal", "paypal", "paypal-secret"),
    ]))
}

async fn unlock_vault() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn list_lanes(Query(q): Query<ShelfQuery>) -> Json<Value> {
    Json(match q.shelf.as_deref() {
        Some("alpha") => json!([
            lane_row("l1", "alpha-one", "alpha"),
            lane_row("l2", "alpha-two", "alpha"),
        ]),
        Some("empty") => json!([]),
        _ => json!([]),
    })
}

async fn list_lane_stocks(Query(q): Query<ShelfQuery>) -> Json<Value> {
    Json(match q.shelf.as_deref() {
        Some("mine") => json!([
            lane_row("s1", "stock-one", "mine"),
            lane_row("s2", "stock-two", "mine"),
            // Shared title with LangLane{shelf=alpha} so RA-13 A minus B drops a row.
            lane_row("s3", "alpha-one", "mine"),
        ]),
        Some("empty") => json!([]),
        _ => json!([]),
    })
}

async fn spawn_hermit_host_root(spec_path: &std::path::Path) -> String {
    spawn_hermit_host_root_with_bounds(spec_path, 1, 5).await
}

async fn spawn_hermit_host_root_with_bounds(
    spec_path: &std::path::Path,
    min: usize,
    max: usize,
) -> String {
    spawn_hermit_host_root_configured(spec_path, min, max, false).await
}

async fn spawn_hermit_host_root_configured(
    spec_path: &std::path::Path,
    min: usize,
    max: usize,
    deterministic_items: bool,
) -> String {
    // Hermit's schema faker may emit sparse objects; `for_each` templates read row JSON (`_.id`).
    // Prefer declared OpenAPI `example` payloads so list GETs return stable primary keys (i1, i2, …).
    beavuck_hermit::resource_generator::set_use_examples(true);
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let hermit_router = beavuck_hermit::router::build_with_bounds(routes, min, max);
    let lab = CursorLab {
        store: Arc::new(Mutex::new(HashMap::new())),
    };
    // Stateful cursor routes take precedence; unmatched paths fall through to Hermit.
    let sidecar = language_matrix_sidecar(lab);
    let sidecar = if deterministic_items {
        sidecar.merge(parity_items::router(spec_path))
    } else {
        sidecar
    };
    let router = sidecar.fallback_service(hermit_router);

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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub async fn language_matrix_reset_lang_cursors_on(base: &str) {
    let _ = reqwest::Client::new()
        .post(format!("{base}/language/v1/cursors/_lab_reset"))
        .send()
        .await;
}

/// Isolated backend for Python/Plasm differential read conformance.
#[allow(dead_code)] // Shared fixture helper; not every integration binary uses it.
pub async fn language_matrix_python_hermit_base_url() -> &'static String {
    static PYTHON_MATRIX: OnceCell<String> = OnceCell::const_new();
    hermit_base_url(&PYTHON_MATRIX).await
}

/// Fresh state for each frontend in write differential cases.
#[allow(dead_code)] // Shared fixture helper; not every integration binary uses it.
pub async fn fresh_language_matrix_hermit_base_url() -> String {
    // Fix collection cardinality across independent generators; payload projection excludes faker identities.
    spawn_hermit_host_root_with_bounds(language_matrix_spec_path().as_path(), 2, 2).await
}

/// Independent identical state, including real item writes, for each frontend.
#[allow(dead_code)] // Shared fixture helper; not every integration binary uses it.
pub async fn fresh_python_parity_hermit_base_url() -> String {
    spawn_hermit_host_root_configured(language_matrix_spec_path().as_path(), 5, 5, true).await
}
