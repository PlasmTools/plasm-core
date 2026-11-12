//! A relation read must conserve the write that produced its parent.
use plasm_core::{ChainExpr, CreateExpr, Expr, Value};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::sync::{Arc, Mutex};

#[path = "common/language_matrix.rs"]
mod language_matrix;

#[tokio::test]
async fn create_then_relation_conserves_one_write_across_fanout() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let empty = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let empty_response = empty.clone();
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let seen = seen.clone();
        let empty_response = empty_response.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            seen.lock().unwrap().push((method.clone(), path.clone()));
            let body = if method == axum::http::Method::POST {
                serde_json::json!({"id":"i1", "title":"created", "score":1, "owner":"a", "lines":[{"id":"l1","item_id":"i1","note":"one"},{"id":"l2","item_id":"i1","note":"two"}], "summary":{"id":"s1","headline":"summary"}})
            } else if path.ends_with("/tags") {
                if empty_response.load(std::sync::atomic::Ordering::SeqCst) {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{"id":"t1","item_id":"i1","label":"tag"}])
                }
            } else if path.ends_with("/l1") {
                serde_json::json!({"id":"l1","item_id":"i1","note":"one"})
            } else if path.ends_with("/l2") {
                serde_json::json!({"id":"l2","item_id":"i1","note":"two"})
            } else if path.ends_with("/items/i1") {
                serde_json::json!({"id":"i1","title":"created","score":1,"owner":"a"})
            } else {
                serde_json::json!({"id":"s1","headline":"summary"})
            };
            axum::Json(body)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    for (fixture, relation, is_empty) in [
        ("plasm_language_matrix", "summary", false),
        ("plasm_language_matrix", "lines", false),
        ("plasm_language_matrix", "tags", false),
        ("plasm_language_matrix", "tags_by_score", false),
        ("plasm_language_matrix_views", "self_via_bindings", false),
        ("plasm_language_matrix", "tags_by_score", true),
    ] {
        empty.store(is_empty, std::sync::atomic::Ordering::SeqCst);
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(plasm_e2e::test_manifest_dir())
                .join("../../fixtures/schemas")
                .join(fixture),
        )
        .unwrap();
        let create = Expr::Create(CreateExpr::new(
            "langitem_create",
            "LangItem",
            Value::Object(
                [
                    ("title".into(), Value::String("created".into())),
                    ("score".into(), Value::Integer(1)),
                    ("owner".into(), Value::String("a".into())),
                ]
                .into(),
            ),
        ));
        let expr = Expr::Chain(ChainExpr::auto_get(create, relation));
        let result = engine
            .execute(
                &expr,
                &cgs,
                &mut SessionMaterialization::new(),
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        let entries = result.operations.entries();
        assert_eq!(
            result.count() == 0,
            is_empty,
            "{relation}: unexpected child rows"
        );
        assert_eq!(entries.len(), 1, "{relation}: {entries:?}");
        assert_eq!(entries[0].capability, "langitem_create");
        assert_eq!(
            (
                entries[0].logical_invocations,
                entries[0].completed,
                entries[0].failed
            ),
            (1, 1, 0)
        );
    }
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| *method == axum::http::Method::POST)
            .count(),
        6
    );
    server.abort();
}

#[tokio::test]
async fn python_create_then_relation_publishes_the_write_once() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let seen = seen.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            seen.lock().unwrap().push((method.clone(), path.clone()));
            if path.ends_with("/tags") {
                axum::Json(serde_json::json!([{"id":"t1","item_id":"i1","label":"one"},{"id":"t2","item_id":"i1","label":"two"}]))
            } else if path.ends_with("/tags/t1") {
                axum::Json(serde_json::json!({"id":"t1","item_id":"i1","label":"one"}))
            } else if path.ends_with("/tags/t2") {
                axum::Json(serde_json::json!({"id":"t2","item_id":"i1","label":"two"}))
            } else {
                axum::Json(serde_json::json!({"id":"i1", "title":"created", "score":1, "owner":"a"}))
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs.clone());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let create = symbols.method_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "create");
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "tags");
    let program = format!("class Ledger(Program):\n    def build(self):\n        parent = {entity}.{create}(title=\"created\", score=1, owner=\"a\")\n        return parent.{relation}\n");
    let bundle = plasm_agent::plasm_compile::compile_python_program(&es, &program)
        .await
        .unwrap();
    let dry = plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    assert!(requests.lock().unwrap().is_empty());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let host = language_matrix::matrix_host_state(engine, cgs);
    let run = plasm_agent::plasm_plan_run::run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "ledger",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
    )
    .await
    .unwrap();
    let writes: Vec<_> = run
        .return_steps
        .iter()
        .flat_map(|step| step.result.operations.entries())
        .filter(|ack| ack.capability == "langitem_create")
        .collect();
    assert!(run.return_steps.iter().any(|step| step.result.count() == 2));
    assert_eq!(writes.len(), 1, "{:?}", run.return_steps);
    assert_eq!(
        (
            writes[0].logical_invocations,
            writes[0].completed,
            writes[0].failed
        ),
        (1, 1, 0)
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| *method == axum::http::Method::POST)
            .count(),
        1
    );
    server.abort();
}
