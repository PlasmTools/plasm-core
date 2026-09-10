//! L1 stub-HTTP pagination matrix: real CGS/CML → compile → HTTP → driver → materialization.
//!
//! The stub records each request's pagination parameters and slices a deterministic entity set.
//! Indexed mode uses `start = page_index * page_limit` so a historical `20 → 5` mutation would
//! deterministically reproduce overlapping ids (regression guard).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use plasm_core::{Expr, QueryExpr, QueryPagination, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, LiveRunTelemetry,
    SessionMaterialization, StreamConsumeOpts,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct StubState {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    query: HashMap<String, String>,
}

fn load_matrix_cgs() -> CGS {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        root.join("../../fixtures/schemas/plasm_pagination_matrix"),
        root.join("../../../fixtures/schemas/plasm_pagination_matrix"),
    ];
    for p in &candidates {
        if p.exists() {
            return plasm_core::loader::load_schema_dir(p).expect("pagination matrix CGS");
        }
    }
    panic!("fixtures/schemas/plasm_pagination_matrix not found");
}

fn items(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            serde_json::json!({
                "id": format!("id-{i}"),
                "n": i,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct IndexedParams {
    page_index: Option<u64>,
    page_limit: Option<u64>,
}

async fn indexed(
    State(st): State<StubState>,
    Query(q): Query<IndexedParams>,
) -> Json<serde_json::Value> {
    let mut map = HashMap::new();
    if let Some(v) = q.page_index {
        map.insert("page_index".into(), v.to_string());
    }
    if let Some(v) = q.page_limit {
        map.insert("page_limit".into(), v.to_string());
    }
    st.requests
        .lock()
        .unwrap()
        .push(RecordedRequest { query: map });
    let all = items(45);
    let limit = q.page_limit.unwrap_or(20) as usize;
    let index = q.page_index.unwrap_or(0) as usize;
    // Historical corruption: start = page_index * page_limit (not remaining budget).
    let start = index.saturating_mul(limit);
    let page = all.into_iter().skip(start).take(limit).collect::<Vec<_>>();
    Json(serde_json::json!({ "results": page }))
}

#[derive(Debug, Deserialize)]
struct OffsetParams {
    offset: Option<u64>,
    limit: Option<u64>,
}

async fn offset(
    State(st): State<StubState>,
    Query(q): Query<OffsetParams>,
) -> Json<serde_json::Value> {
    let mut map = HashMap::new();
    if let Some(v) = q.offset {
        map.insert("offset".into(), v.to_string());
    }
    if let Some(v) = q.limit {
        map.insert("limit".into(), v.to_string());
    }
    st.requests
        .lock()
        .unwrap()
        .push(RecordedRequest { query: map });
    let all = items(45);
    let limit = q.limit.unwrap_or(20) as usize;
    let start = q.offset.unwrap_or(0) as usize;
    let page = all.into_iter().skip(start).take(limit).collect::<Vec<_>>();
    Json(serde_json::json!({ "results": page }))
}

#[derive(Debug, Deserialize)]
struct CursorParams {
    cursor: Option<String>,
    limit: Option<u64>,
}

async fn cursor(
    State(st): State<StubState>,
    Query(q): Query<CursorParams>,
) -> Json<serde_json::Value> {
    let mut map = HashMap::new();
    if let Some(ref v) = q.cursor {
        map.insert("cursor".into(), v.clone());
    }
    if let Some(v) = q.limit {
        map.insert("limit".into(), v.to_string());
    }
    st.requests
        .lock()
        .unwrap()
        .push(RecordedRequest { query: map });
    let all = items(45);
    let limit = q.limit.unwrap_or(20) as usize;
    let start = q
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix("c"))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let page = all
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next = if start + limit < all.len() {
        Some(format!("c{}", start + limit))
    } else {
        None
    };
    Json(serde_json::json!({ "results": page, "next_cursor": next }))
}

async fn spawn_stub() -> (String, StubState) {
    let st = StubState::default();
    let app = Router::new()
        .route("/items/indexed", get(indexed))
        .route("/items/offset", get(offset))
        .route("/items/cursor", get(cursor))
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), st)
}

fn make_engine(base_url: &str) -> ExecutionEngine {
    ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url.to_string()),
        ..Default::default()
    })
    .unwrap()
}

fn ids(result: &plasm_runtime::ExecutionResult) -> Vec<String> {
    result
        .entities
        .iter()
        .map(|e| e.reference.primary_slot_str())
        .collect()
}

#[tokio::test]
async fn indexed_host_budget_25_keeps_stable_page_limit_20() {
    let (url, stub) = spawn_stub().await;
    let cgs = load_matrix_cgs();
    let engine = make_engine(&url);
    let mut cache = SessionMaterialization::new();
    let tel = Arc::new(LiveRunTelemetry::new());

    let mut query = QueryExpr::all("Item");
    query.capability_name = Some("item_query".into());
    query.pagination = Some(QueryPagination::default());

    let result = plasm_runtime::with_live_run_telemetry(tel.clone(), async {
        engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut cache,
                Some(ExecutionMode::Live),
                StreamConsumeOpts {
                    fetch_all: false,
                    max_items: Some(25),
                    one_page: false,
                    ..Default::default()
                },
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
    })
    .await
    .expect("execute");

    let reqs = stub.requests.lock().unwrap().clone();
    assert!(
        reqs.len() >= 2,
        "expected multiple upstream pages, got {reqs:?}"
    );
    for r in &reqs {
        assert_eq!(
            r.query.get("page_limit").map(String::as_str),
            Some("20"),
            "Fixed page_limit must stay 20: {r:?}"
        );
    }
    assert_eq!(
        reqs[0].query.get("page_index").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        reqs[1].query.get("page_index").map(String::as_str),
        Some("1")
    );

    let got = ids(&result);
    assert_eq!(got.len(), 25);
    let unique: std::collections::BTreeSet<_> = got.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        25,
        "no overlapping identities under host budget 25"
    );
    assert_eq!(got[0], "id-0");
    assert_eq!(got[19], "id-19");
    assert_eq!(got[20], "id-20");
    assert_eq!(got[24], "id-24");

    let audits = tel.drain_page_audits();
    assert!(
        audits.len() >= 2,
        "expected per-page audits, got {}",
        audits.len()
    );
    assert!(audits.iter().all(|a| a.requested_size == 20));
    assert!(audits.iter().all(|a| a.duplicate_rows == 0));
}

#[tokio::test]
async fn indexed_complete_fetch_all_reaches_authoritative_end() {
    let (url, stub) = spawn_stub().await;
    let cgs = load_matrix_cgs();
    let engine = make_engine(&url);
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("Item");
    query.capability_name = Some("item_query".into());
    query.pagination = Some(QueryPagination::default());

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .expect("execute");

    let reqs = stub.requests.lock().unwrap().clone();
    assert!(
        reqs.len() >= 3,
        "45 items / 20 ⇒ ≥3 pages, got {}",
        reqs.len()
    );
    assert!(reqs
        .iter()
        .all(|r| r.query.get("page_limit").map(String::as_str) == Some("20")));
    let got = ids(&result);
    assert_eq!(got.len(), 45);
    let unique: std::collections::BTreeSet<_> = got.iter().cloned().collect();
    assert_eq!(unique.len(), 45);
}

#[tokio::test]
async fn offset_stride_matches_page_size() {
    let (url, stub) = spawn_stub().await;
    let cgs = load_matrix_cgs();
    let engine = make_engine(&url);
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("ItemOffset");
    query.capability_name = Some("item_offset_query".into());
    query.pagination = Some(QueryPagination::default());

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .expect("execute");

    let reqs = stub.requests.lock().unwrap().clone();
    let offsets: Vec<_> = reqs
        .iter()
        .filter_map(|r| r.query.get("offset").cloned())
        .collect();
    assert!(
        offsets
            .windows(2)
            .all(|w| { w[1].parse::<i64>().unwrap() - w[0].parse::<i64>().unwrap() == 20 }),
        "offset stride must equal page size: {offsets:?}"
    );
    assert_eq!(ids(&result).len(), 45);
}

#[tokio::test]
async fn cursor_strategy_fetches_to_exhaustion() {
    let (url, _stub) = spawn_stub().await;
    let cgs = load_matrix_cgs();
    let engine = make_engine(&url);
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("ItemCursor");
    query.capability_name = Some("item_cursor_query".into());
    query.pagination = Some(QueryPagination::default());

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .expect("execute");
    assert_eq!(ids(&result).len(), 45);
}
