//! Query source bindings and row predicates across planning and execution.
use plasm_core::{Expr, Predicate, QueryExpr};
use plasm_runtime::{
    execution::preflight_compile_expr, ExecuteOptions, ExecutionConfig, ExecutionEngine,
    ExecutionMode, SessionMaterialization, StreamConsumeOpts, ViewAmbientContext,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
#[path = "common/language_matrix_views.rs"]
#[allow(dead_code)]
mod language_matrix_views;

#[tokio::test]
async fn source_disjunction_is_rejected_before_dispatch_in_preflight_and_live() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = axum::Router::new().fallback(axum::routing::get(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            axum::Json(serde_json::json!([{"id":"other"}]))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/scoped_query_matrix"),
    )
    .unwrap();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    for predicate in [
        Predicate::or(vec![
            Predicate::eq("cohort_id", 7_i64),
            Predicate::eq("id", "wanted"),
        ]),
        Predicate::or(vec![
            Predicate::eq("cohort_id", 7_i64),
            Predicate::eq("cohort_id", 8_i64),
        ]),
        Predicate::negate(Predicate::eq("cohort_id", 7_i64)),
        Predicate::or(vec![Predicate::eq("id", "a"), Predicate::eq("id", "b")]),
    ] {
        let mut query = QueryExpr::all("Member");
        query.capability_name = Some("member_query".into());
        query.predicate = Some(predicate);
        let expr = Expr::Query(query);
        let mut mat = SessionMaterialization::new();
        let preflight =
            preflight_compile_expr(&expr, &cgs, &compiled, &ViewAmbientContext::default(), &mat);
        let live = engine
            .execute(
                &expr,
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await;
        assert!(
            matches!(&live, Err(plasm_runtime::RuntimeError::TypeError { .. })),
            "{live:?}"
        );
        assert!(
            matches!(
                &preflight,
                Err(plasm_runtime::RuntimeError::TypeError { .. })
            ),
            "{preflight:?}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn python_scope_and_row_disjunction_keep_their_separate_laws() {
    use std::sync::Mutex;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = axum::Router::new().fallback(axum::routing::get(move |uri: axum::http::Uri| {
        let observed = observed.clone();
        async move {
            observed.lock().unwrap().push(uri.to_string());
            axum::Json(serde_json::json!([{"id":"a"},{"id":"b"},{"id":"c"}]))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/scoped_query_matrix"),
    )
    .unwrap();
    let entry = language_matrix_views::VIEWS_MATRIX_ENTRY_ID;
    cgs.bind_registry_entry_id(entry);
    let cgs = Arc::new(cgs);
    let session = language_matrix_views::execute_session_for_entities(cgs.clone(), &["Member"]);
    let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(entry, "Member");
    let scope = symbols.ident_sym_cap_param_for(entry, "Member", "member_query", "cohort_id");
    let program = format!("class Example(Program):\n    def build(self):\n        rows = {entity}.query({scope}=7)\n        return rows.where(lambda row: row.id == \"a\" or row.id == \"b\")\n");
    let bundle = plasm_agent::plasm_compile::compile_python_program(&session, &program)
        .await
        .unwrap();
    plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();
    assert!(
        requests.lock().unwrap().is_empty(),
        "dry planning must not perform I/O"
    );
    let host = language_matrix_views::views_matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            hydrate: false,
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &session,
        &host,
        &session.prompt_hash,
        "query-partition",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let rows = result.return_steps.last().unwrap().result.entities();
    let ids: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|row| row.reference.primary_slot_str())
        .collect();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from(["a".to_string(), "b".to_string()])
    );
    let requests = requests.lock().unwrap();
    assert!(
        requests.iter().any(|uri| uri.contains("cohort_id=7")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|uri| !uri.contains("id=a") && !uri.contains("id=b")),
        "{requests:?}"
    );
    server.abort();
}

#[tokio::test]
async fn homonymous_scope_is_not_applied_as_a_row_predicate() {
    let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = paths.clone();
    let app = axum::Router::new().fallback(axum::routing::get(move |uri: axum::http::Uri| {
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(uri.path().to_string());
            axum::Json(serde_json::json!([{"id":"child","parent_id":"parent-other"}]))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/scoped_query_matrix"),
    )
    .unwrap();
    let mut parsed =
        plasm_core::expr_parser::parse("ChildWithParent{parent_id=Parent(\"parent-one\")}", &cgs)
            .unwrap();
    plasm_core::normalize_expr_query_capabilities(&mut parsed.expr, &cgs).unwrap();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut mat = SessionMaterialization::new();
    preflight_compile_expr(
        &parsed.expr,
        &cgs,
        &compiled,
        &ViewAmbientContext::default(),
        &mat,
    )
    .unwrap();
    assert!(paths.lock().unwrap().is_empty());
    let result = engine
        .execute(
            &parsed.expr,
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.count(),
        1,
        "scope binding must not filter the similarly named row field"
    );
    assert_eq!(&*paths.lock().unwrap(), &["/parents/parent-one/children"]);
    server.abort();
}
