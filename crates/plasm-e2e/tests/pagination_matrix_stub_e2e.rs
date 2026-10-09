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
    summaries: bool,
    details: Arc<Mutex<Vec<String>>>,
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
    Json(
        serde_json::json!({ "results": if st.summaries { page.into_iter().map(|r| serde_json::json!({"id":r["id"]})).collect::<Vec<_>>() } else { page } }),
    )
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
    spawn_stub_with(StubState::default()).await
}

async fn spawn_stub_with(st: StubState) -> (String, StubState) {
    let app = Router::new()
        .route("/items/indexed", get(indexed))
        .route("/items/indexed/{id}", get(indexed_detail))
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
        .entities()
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

#[path = "common/language_matrix.rs"]
mod language_matrix;

#[test]
fn row_algebra_reads_matches_beyond_the_first_backend_pages() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                use plasm_agent::plasm_compile::compile_plasm_program;
                use plasm_agent::plasm_plan_run::run_plasm_comp;
                use plasm_runtime::ResultCoverage;
                for (program, expected) in [
                    ("rows = Item\nrows | where n >= 40 | select id, n", 5),
                    ("rows = Item\nrows | select id, n", 45),
                    ("rows = Item | take 3\nrows | select id, n", 3),
                ] {
                    let (base, recorded) = spawn_stub().await;
                    let mut cgs = load_matrix_cgs();
                    cgs.http_backend = base.clone();
                    let cgs = Arc::new(cgs);
                    let session = language_matrix::matrix_execute_session(cgs.clone());
                    let host = language_matrix::matrix_host_state(
                        ExecutionEngine::new(ExecutionConfig {
                            base_url: Some(base),
                            hydrate: false,
                            ..Default::default()
                        })
                        .unwrap(),
                        cgs,
                    );
                    let bundle = compile_plasm_program(
                        &Default::default(),
                        None,
                        &session,
                        "algebra",
                        program,
                    )
                    .expect("compile algebra");
                    let result = Box::pin(run_plasm_comp(
                        &session,
                        &host,
                        &session.prompt_hash,
                        "algebra",
                        &bundle,
                        true,
                        None,
                        None,
                        None,
                        None,
                    ))
                    .await
                    .expect("execute algebra");
                    let output = &result.return_steps[0].result;
                    // Delivery paging limits resident rows, not the immutable
                    // semantic collection or its completeness evidence.
                    assert_eq!(output.count(), expected, "{program}");
                    assert_eq!(output.entities().len(), expected.min(25), "{program}");
                    assert_eq!(output.coverage(), ResultCoverage::Complete, "{program}");
                    assert_eq!(output.has_more, expected > 25, "{program}");
                    assert_eq!(output.paging_handle.is_some(), expected > 25, "{program}");
                    assert_eq!(
                        output
                            .collection
                            .computation_source()
                            .materialize(plasm_core::collection_codec::Demand::Whole)
                            .expect("complete immutable computation source")
                            .len(),
                        expected,
                        "{program}"
                    );
                    let requests = recorded.requests.lock().unwrap();
                    assert_eq!(
                        requests.len(),
                        if expected == 3 { 1 } else { 3 },
                        "bounded take versus exhaustive algebra: {program}"
                    );
                }
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn indexed_detail(
    State(st): State<StubState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    st.details.lock().unwrap().push(id.clone());
    let n: usize = id.trim_start_matches("id-").parse().unwrap();
    Json(serde_json::json!({"id":id,"n":n}))
}

#[tokio::test]
async fn expression_take_hydrates_only_retained_prefix() {
    let (url, stub) = spawn_stub_with(StubState {
        summaries: true,
        ..Default::default()
    })
    .await;
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
                max_items: Some(1),
                bound_kind: plasm_runtime::ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ids(&result), vec!["id-0"]);
    assert_eq!(
        *stub.details.lock().unwrap(),
        vec!["id-0"],
        "take must precede unnecessary detail IO"
    );
    assert_eq!(stub.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn hydration_prefix_preserves_selection_and_page_boundaries() {
    use plasm_runtime::{ConsumeBoundKind, RowMatchBudget, TopKSpec};
    for (name, consume, expected_ids, details, pages) in [
        (
            "zero",
            StreamConsumeOpts {
                max_items: Some(0),
                bound_kind: ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            vec![],
            0,
            0,
        ),
        (
            "cross_page",
            StreamConsumeOpts {
                max_items: Some(25),
                bound_kind: ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            (0..25).collect(),
            25,
            2,
        ),
        (
            "empty_predicate_budget",
            StreamConsumeOpts {
                fetch_all: true,
                row_match_budget: Some(RowMatchBudget {
                    count: 1,
                    predicates: vec![],
                }),
                bound_kind: ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            vec![0],
            1,
            1,
        ),
        (
            "host_page",
            StreamConsumeOpts {
                max_items: Some(1),
                bound_kind: ConsumeBoundKind::HostPage,
                ..Default::default()
            },
            (0..20).collect(),
            20,
            1,
        ),
        (
            "filtered_prefix",
            StreamConsumeOpts {
                fetch_all: true,
                row_match_budget: Some(RowMatchBudget {
                    count: 1,
                    predicates: vec![plasm_runtime::BoundRowPredicate {
                        field_path: plasm_core::FieldPath::from_dotted("n").unwrap(),
                        op: plasm_core::PlanPredicateOp::Eq,
                        value: plasm_core::operand_binding::ResolvedValue::new(
                            plasm_core::Value::Integer(21),
                        )
                        .unwrap(),
                    }],
                }),
                bound_kind: ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            vec![21],
            40,
            2,
        ),
        (
            "top_k",
            StreamConsumeOpts {
                fetch_all: true,
                top_k: Some(TopKSpec {
                    count: 1,
                    sort_key: vec!["n".into()],
                    descending: true,
                    row_filter: vec![],
                }),
                bound_kind: ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            vec![44],
            45,
            3,
        ),
    ] {
        let (url, stub) = spawn_stub_with(StubState {
            summaries: true,
            ..Default::default()
        })
        .await;
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
                consume,
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            ids(&result),
            expected_ids
                .into_iter()
                .map(|i| format!("id-{i}"))
                .collect::<Vec<_>>(),
            "{name}"
        );
        assert_eq!(
            stub.details.lock().unwrap().len(),
            details,
            "{name} detail count"
        );
        assert_eq!(
            stub.requests.lock().unwrap().len(),
            pages,
            "{name} page count"
        );
    }
}
