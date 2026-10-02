//! Callback laws use abstract catalogs and real scoped DAG execution.
use super::python::{compile_fixture, parity_context, Case};

#[tokio::test]
async fn callbacks_admit_lexical_bindings_at_rowset_consumers() {
    let case = Case {
        id: "callbacks",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, _) = parity_context(&case, "http://127.0.0.1:1");
    for body in [
        "class P(Program):\n    def build(self):\n        def choose(row) -> str:\n            return 'fixed'\n        return E.query().select(label=choose)",

        "def choose(row) -> dict[str, str]:\n    return {'id': row.id}\nreturn E.query().map(choose, max_parents=8)",
        "return E.query().select(label=lambda row, prefix='tag:': prefix + row.title)",

        "def choose(row, /, *, prefix='label:') -> str:\n    return prefix + row.title\nreturn E.query().select(label=choose)",
        "def choose(row, optional=1) -> str:\n    return row.title\nreturn E.query().where(choose)",
        "def choose(row):\n    if True:\n        return {'id': row.id}\n    return {'wrong': row.absent}\nreturn E.query().map(choose, max_parents=8)",
        "def act(row) -> None:\n    row.PING()\nreturn E.query().flat_map(act)",
        "def act(row):\n    if row.active:\n        row.PING()\nreturn E.query().flat_map(act)",
        "def act(row):\n    value = 1\n    value = 2\n    return {'value': value}\nreturn E.query().map(act, max_parents=8)",
        "def select_row(row):\n    value = row.score\n    return value is not None and value > 10\nreturn E.query().where(select_row)",
        "prefix = 'label:'\ndef render_row(row):\n    title = prefix + row.title\n    return {'title': title}\nreturn E.query().map(render_row, max_parents=8)",
        "def project(row):\n    record = {'id': row.id}\n    return record\nreturn E.query().map(project, max_parents=8)",
        "def project(row):\n    if row.active:\n        return {'id': row.id}\n    return {'id': row.title}\nreturn E.query().map(project, max_parents=8)",
        "def choose(row):\n    if row.active:\n        return row.title\n    return None\nreturn E.query().where(choose)",
        "def act(row):\n    result = row.PING()\n    return result\nreturn E.query().flat_map(act)",
    ] {
        compile_fixture(&es, body).await.unwrap_or_else(|e| panic!("{body}\n{e}"));
    }
}

#[tokio::test]
async fn callbacks_resolve_callable_names_at_definition_site() {
    let case = Case {
        id: "callbacks",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, _) = parity_context(&case, "http://127.0.0.1:1");
    let caller_only = "def outer(row):\n    return helper(row)\ndef caller(row):\n    def helper(member):\n        return member.id\n    return {'id': outer(row)}\nreturn E.query().map(caller, max_parents=8)";
    assert!(
        compile_fixture(&es, caller_only).await.is_err(),
        "a caller-local helper must not satisfy an outer callback's free name"
    );
    for body in [
        "def helper(row):\n    return row.id\ndef outer(row):\n    return helper(row)\ndef caller(row):\n    return {'id': outer(row)}\nreturn E.query().map(caller, max_parents=8)",
        "def caller(row):\n    def helper(member):\n        return member.id\n    def outer(member):\n        return helper(member)\n    return {'id': outer(row)}\nreturn E.query().map(caller, max_parents=8)",
        "def helper(row):\n    return row.id\ndef caller(row):\n    if row.active:\n        return {'id': helper(row)}\n    return {'id': helper(row)}\nreturn E.query().map(caller, max_parents=8)",
    ] {
        compile_fixture(&es, body)
            .await
            .unwrap_or_else(|error| panic!("{body}\n{error}"));
    }
    let recursive =
        "def act(row):\n    return E.query().flat_map(act)\nreturn E.query().flat_map(act)";
    let error = compile_fixture(&es, recursive).await.unwrap_err();
    assert!(error.contains("recursive callbacks"), "{error}");
}

#[tokio::test]
async fn callbacks_live_conditional_effects() {
    use super::evaluate_plasm_comp_dry;
    use axum::{
        extract::Path,
        routing::{get, post},
        Json, Router,
    };
    use plasm_agent::plasm_plan_run::run_plasm_comp;
    use std::sync::Arc;
    use std::sync::Mutex;
    // Observe real HTTP writes independently of plan receipts or result shaping.
    let writes = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = writes.clone();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fail_capture = fail.clone();
    let reads = Arc::new(Mutex::new(Vec::<String>::new()));
    let read_capture = reads.clone();
    let app = Router::new()
        .route(
            "/language/v1/items",
            get(|| async {
                Json(serde_json::json!([
                    {"id":"yes", "title":"Selected"},
                    {"id":"no", "title":"Excluded"}
                ]))
            }),
        )
        .route(
            "/language/v1/items/{id}",
            get(move |Path(id): Path<String>| {
                let reads = read_capture.clone();
                async move {
                    reads.lock().unwrap().push(id.clone());
                    Json(serde_json::json!({"id":id, "title":"Item", "active":id == "yes"}))
                }
            }),
        )
        .route(
            "/language/v1/items/{id}/ping",
            post(move |Path(id): Path<String>| {
                let captured = captured.clone();
                let fail = fail_capture.clone();
                async move {
                    captured.lock().unwrap().push(id.clone());
                    let status = if fail.load(std::sync::atomic::Ordering::SeqCst) {
                        axum::http::StatusCode::CONFLICT
                    } else {
                        axum::http::StatusCode::OK
                    };
                    (
                        status,
                        Json(serde_json::json!({"ok":status.is_success(), "id":id})),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (body, expected, fails) in [
        ("flag = False\ndef act(row, *, enabled: bool = flag):\n    if enabled:\n        row.PING()\nflag = True\nreturn E.query().flat_map(act)", vec![], false),
        ("def act(row) -> None:\n    if row.active:\n        row.PING()\nreturn E.query().flat_map(act)", vec!["yes"], false),
        ("def act(row):\n    if False:\n        row.PING()\n    row.PING()\nreturn E.query().flat_map(act)", vec!["yes", "no"], false),
        ("def act(row):\n    member = E.get(row.id)\n    if member.active:\n        return member.PING()\n    return None\nreturn E.query().flat_map(act)", vec!["yes"], false),
        ("def act(row):\n    if row.active:\n        return row.PING()\n    else:\n        return row.PING()\nreturn E.query().flat_map(act)", vec!["yes", "no"], false),
        ("def act(row):\n    if row.active:\n        return None\n    row.PING()\n    return None\nreturn E.query().flat_map(act)", vec!["no"], false),
        ("def act(row):\n    if row.active:\n        return None\n    elif row.title:\n        return row.PING()\n    return None\nreturn E.query().flat_map(act)", vec!["no"], false),
        ("def act(row):\n    row.PING()\n    return row.PING()\nreturn E.query().flat_map(act)", vec!["yes", "yes", "no", "no"], false),
        ("def act(row):\n    row.PING()\n    return row.PING()\nreturn E.query().flat_map(act)", vec!["yes"], true),
        ("def act(row):\n    return row.PING()\nreturn E.query().take(0).flat_map(act)", vec![], false),
    ] {
        fail.store(fails, std::sync::atomic::Ordering::SeqCst);
        writes.lock().unwrap().clear();
        reads.lock().unwrap().clear();
        let case = Case {
            id: "nominal_boolean_effects",
            python: "",
            existing: None,
            expect_live_error: None,
        };
        let (es, host) = parity_context(&case, &base);
        let bundle = compile_fixture(&es, body).await.unwrap_or_else(|e| panic!("{body}: {e}"));
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "planning dispatched an effect"
        );
        let result = Box::pin(run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            "nominal-boolean-effects",
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await;
        if fails { assert!(result.is_err(), "expected service failure"); }
        else {
            let run = result.unwrap_or_else(|e| panic!("{body}: {e}"));
            let completed: usize = run.return_steps[0].result.operations.entries().iter().map(|ack| ack.completed).sum();
            assert_eq!(completed, expected.len(), "effect receipts: {body}");
        }
        assert_eq!(
            *writes.lock().unwrap(),
            expected,
            "{body}"
        );
        // Graph re-observation after writes can hydrate an entity again;
        // logical binding evaluation is distinct from transport request count.
        assert!(reads.lock().unwrap().iter().all(|id| id == "yes" || id == "no"));
    }
    server.abort();
}

#[tokio::test]
async fn callbacks_live_lexical_record() {
    super::python::run_python_cases(
        super::python::CASES
            .iter()
            .filter(|c| c.id.starts_with("callback_")),
    )
    .await;
}

#[tokio::test]
async fn callbacks_reject_unbounded_or_unbound_forms() {
    let case = Case {
        id: "callbacks",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, _) = parity_context(&case, "http://127.0.0.1:1");
    for body in [
        "def act(row, self=1):\n    return row.title\nreturn E.query().where(act)",

        "def act(row, *, n: int = 'bad'):\n    return row.title\nreturn E.query().where(act)",

        "def act(row, required):\n    return row.title\nreturn E.query().where(act)",
        "def act(row, *, required):\n    return row.title\nreturn E.query().where(act)",
        "def act(row) -> int:\n    return row.title\nreturn E.query().where(act)",

        "def act(row):\n    row.PING()\n    return True\nreturn E.query().where(act)",
        "def act(row):\n    row.PING()\n    return row.id\nreturn E.query().select(id=act)",
        "def act(row):\n    return E.query().flat_map(act)\nreturn E.query().flat_map(act)",
        "value = 1\ndef act(row):\n    answer = value\n    value = 2\n    return {'answer': answer}\nreturn E.query().map(act, max_parents=8)",
        "def act(row):\n    if row.active:\n        value = 1\n    return {'value': value}\nreturn E.query().map(act, max_parents=8)",
    ] {
        assert!(compile_fixture(&es, body).await.is_err(), "unexpected admission: {body}");
    }
}

#[tokio::test]
async fn callbacks_preserve_existing_lambda_contracts() {
    super::python::run_python_cases(super::python::CASES.iter().filter(|c| {
        matches!(
            c.id,
            "scoped_nested_effects"
                | "scoped_flat_map_format"
                | "value_recursive_conditional"
                | "predicate_truth_refinement"
                | "predicate_truth_iteration"
                | "root_build_statements"
        )
    }))
    .await;
}

#[tokio::test]
async fn callbacks_declaration_erasure_and_alpha_renaming_preserve_semantics() {
    let case = Case {
        id: "callbacks",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, _) = parity_context(&case, "http://127.0.0.1:1");
    let plain = compile_fixture(&es, "return E.get('i1')").await.unwrap();
    let unused = compile_fixture(
        &es,
        "def unused(row):\n    return row.PING()\nreturn E.get('i1')",
    )
    .await
    .unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &plain.artifact().comp,
        &unused.artifact().comp
    ));
    let original = compile_fixture(&es, "def project(row):\n    key = row.id\n    return {'id': key}\nreturn E.get('i1').map(project, max_parents=1)").await.unwrap();
    for function in ["select_row", "render_row", "project"] {
        for parameter in ["item", "entry", "row"] {
            let source = format!("def {function}({parameter}):\n    identifier = {parameter}.id\n    return {{'id': identifier}}\nreturn E.get('i1').map({function}, max_parents=1)");
            let renamed = compile_fixture(&es, &source).await.unwrap();
            assert!(
                plasm_core::plasm_monad::comp_semantic_eq(
                    &original.artifact().comp,
                    &renamed.artifact().comp
                ),
                "{source}"
            );
        }
    }
}
