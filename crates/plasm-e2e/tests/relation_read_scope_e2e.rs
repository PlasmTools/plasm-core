//! Relation reads must respect the scope of each parent occurrence.
use plasm_core::{ChainExpr, Expr, GetExpr, InvokeExpr, Ref, Value};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::sync::{Arc, Mutex};

fn scoped_get(entity: &str, id: &str, token: &str) -> GetExpr {
    let mut get = GetExpr::from_ref(Ref::new(entity, id));
    get.input =
        Some(Value::Object([("access_token".into(), Value::String(token.into()))].into()).into());
    get
}

async fn execute(
    engine: &ExecutionEngine,
    cgs: &plasm_core::CGS,
    mat: &mut SessionMaterialization,
    expr: Expr,
) -> plasm_runtime::ExecutionResult {
    engine
        .execute(
            &expr,
            cgs,
            mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::for_catalog(cgs).unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn relation_read_reobserves_the_same_child_in_the_parent_scope() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let app = axum::Router::new().fallback(axum::routing::get(
        move |request: axum::extract::Request| {
            let seen = seen.clone();
            async move {
                let path = request.uri().path().to_owned();
                let token = request
                    .headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                let hint = request
                    .headers()
                    .get("x-hint")
                    .map(|value| value.to_str().unwrap().to_owned());
                seen.lock()
                    .unwrap()
                    .push((path.clone(), token.clone(), hint));
                if path.starts_with("/folders/") {
                    axum::Json(serde_json::json!({"id":"f", "notes":[{"note_id":1}]}))
                } else {
                    axum::Json(serde_json::json!({"note_id":1,"title":token}))
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/hydration_boundary_matrix"),
    )
    .unwrap();
    let cap = cgs.capabilities.get_mut("note_get").unwrap();
    let plasm_core::InputType::Object { fields, .. } =
        &mut cap.inputs.arguments.as_mut().unwrap().input_type
    else {
        panic!("object input")
    };
    let mut optional = fields[0].clone();
    optional.name = "optional_hint".into();
    optional.required = false;
    fields.push(optional);
    cap.mapping.as_mut().unwrap().template.0["headers"]["fields"].as_array_mut().unwrap().push(serde_json::json!(["X-Hint", {"type":"if", "condition":{"type":"exists","var":"optional_hint"}, "then_expr":{"type":"var","name":"optional_hint"}, "else_expr":{"type":"const","value":null}}]));
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut mat = SessionMaterialization::new();
    let mut initial = scoped_get("Note", "1", "scope-a");
    initial.input = Some(
        Value::Object(
            [
                ("access_token".into(), Value::String("scope-a".into())),
                ("optional_hint".into(), Value::String("only-a".into())),
            ]
            .into(),
        )
        .into(),
    );
    execute(&engine, &cgs, &mut mat, Expr::Get(initial)).await;
    let chain = Expr::Chain(ChainExpr::auto_get(
        Expr::Get(scoped_get("Folder", "f", "scope-b")),
        "notes",
    ));
    let parent = execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(scoped_get("Folder", "f", "scope-b")),
    )
    .await;
    let mut bad_cgs = cgs.clone();
    bad_cgs
        .capabilities
        .get_mut("note_get")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["headers"]["fields"][0][1] = serde_json::json!({"type":"if", "condition":{"type":"exists","var":"access_token"}, "then_expr":{"type":"var","name":"missing_relation_input"}, "else_expr":{"type":"var","name":"access_token"}});
    let bad_compiled = plasm_compile::compile_cgs_capability_templates(&bad_cgs).unwrap();
    let error = plasm_runtime::execution::preflight_compile_bound_relation(
        &chain,
        &parent,
        &bad_cgs,
        &bad_compiled,
        &plasm_runtime::ViewAmbientContext::default(),
        &mat,
    )
    .unwrap_err();
    assert!(
        matches!(error, plasm_runtime::RuntimeError::CmlError { source: plasm_compile::CmlError::VariableNotFound { name } } if name == "missing_relation_input")
    );
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let before_preflight = requests.lock().unwrap().len();
    plasm_runtime::execution::preflight_compile_bound_relation(
        &chain,
        &parent,
        &cgs,
        &compiled,
        &plasm_runtime::ViewAmbientContext::default(),
        &mat,
    )
    .unwrap();
    assert_eq!(requests.lock().unwrap().len(), before_preflight);
    let result = execute(&engine, &cgs, &mut mat, chain).await;
    assert_eq!(
        result.entities()[0].fields["title"].to_value(),
        Value::String("scope-b".into())
    );
    let implicit = execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(GetExpr::new("Note", "1")),
    )
    .await;
    assert_eq!(
        implicit.entities()[0].fields["title"].to_value(),
        Value::String("scope-b".into())
    );
    let cached = execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(scoped_get("Note", "1", "scope-b")),
    )
    .await;
    assert_eq!(
        cached.entities()[0].fields["title"].to_value(),
        Value::String("scope-b".into())
    );
    let changed = execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(scoped_get("Note", "1", "scope-a")),
    )
    .await;
    assert_eq!(
        changed.entities()[0].fields["title"].to_value(),
        Value::String("scope-a".into())
    );
    let seen = requests.lock().unwrap();
    assert_eq!(
        &seen[..3],
        &[
            ("/notes/1".into(), "scope-a".into(), Some("only-a".into())),
            ("/folders/f".into(), "scope-b".into(), None),
            ("/notes/1".into(), "scope-b".into(), None)
        ]
    );
    assert!(seen[3..]
        .iter()
        .all(|(path, token, hint)| path == "/notes/1"
            && (token == "scope-a" || token == "scope-b")
            && hint.is_none()));
    drop(seen);
    server.abort();
}

#[tokio::test]
async fn relation_read_after_mutation_reobserves_the_child() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = requests.clone();
    let state = changed.clone();
    let app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let seen = seen.clone();
            let state = state.clone();
            async move {
                let path = request.uri().path().to_owned();
                seen.lock().unwrap().push(path.clone());
                if path == "/login" {
                    state.store(true, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(serde_json::json!({"access_token":"scope-a"}))
                } else if path.starts_with("/folders/") {
                    axum::Json(serde_json::json!({"id":"f", "notes":[{"note_id":1}]}))
                } else {
                    let title = if state.load(std::sync::atomic::Ordering::SeqCst) {
                        "after"
                    } else {
                        "before"
                    };
                    axum::Json(serde_json::json!({"note_id":1,"title":title}))
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/hydration_boundary_matrix"),
    )
    .unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut mat = SessionMaterialization::new();
    execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(scoped_get("Note", "1", "scope-a")),
    )
    .await;
    let parent = execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Get(scoped_get("Folder", "f", "scope-a")),
    )
    .await;
    execute(
        &engine,
        &cgs,
        &mut mat,
        Expr::Invoke(InvokeExpr::new("login", "Session", "", None)),
    )
    .await;
    let chain = ChainExpr::auto_get(Expr::Get(scoped_get("Folder", "f", "scope-a")), "notes");
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let before_preflight = requests.lock().unwrap().len();
    plasm_runtime::execution::preflight_compile_bound_relation(
        &Expr::Chain(chain.clone()),
        &parent,
        &cgs,
        &compiled,
        &plasm_runtime::ViewAmbientContext::default(),
        &mat,
    )
    .unwrap();
    assert_eq!(requests.lock().unwrap().len(), before_preflight);
    let result = engine
        .execute_relation_from_observed(
            &chain,
            parent,
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.entities()[0].fields["title"].to_value(),
        Value::String("after".into())
    );
    assert_eq!(
        *requests.lock().unwrap(),
        vec!["/notes/1", "/folders/f", "/login", "/notes/1"]
    );
    server.abort();
}
