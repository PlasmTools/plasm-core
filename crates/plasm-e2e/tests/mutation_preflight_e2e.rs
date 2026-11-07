//! Mutation preflight and live execution through the same fixture and loopback transport.
use plasm_core::{DeleteExpr, Expr, InvokeExpr, Ref, Value};
use plasm_runtime::{
    execution::preflight_compile_expr, ExecuteOptions, ExecutionConfig, ExecutionEngine,
    ExecutionMode, SessionMaterialization, StreamConsumeOpts, ViewAmbientContext,
};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn inherited_scope_is_available_to_preflight_and_live_mutations() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = requests.clone();
    let app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let received = received.clone();
            async move {
                received
                    .lock()
                    .unwrap()
                    .push((request.method().clone(), request.uri().clone()));
                axum::Json(serde_json::json!({"path":"/notes/a.md","id":25891}))
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/target_identity_matrix"),
    )
    .unwrap();
    for name in ["document_touch", "document_delete"] {
        let template = &mut cgs
            .capabilities
            .get_mut(name)
            .unwrap()
            .mapping
            .as_mut()
            .unwrap()
            .template
            .0;
        template["query"] = serde_json::json!({"type":"object", "fields":[
            ["workspace",{"type":"var","name":"workspace"}],
            ["identity",{"type":"var","name":"id"}]
        ]});
    }
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let target = Ref::new("Document", "/notes/a.md");
    let explicit = Value::Object(
        [
            ("workspace".into(), Value::String("override".into())),
            ("id".into(), Value::String("spoofed".into())),
        ]
        .into_iter()
        .collect(),
    );
    let mut delete_with_input = DeleteExpr::with_target("document_delete", target.clone());
    delete_with_input.input = Some(explicit.clone().into());
    for (expr, expected) in [
        (
            Expr::Delete(DeleteExpr::with_target("document_delete", target.clone())),
            "acme",
        ),
        (
            Expr::Invoke(InvokeExpr::with_target(
                "document_touch",
                target.clone(),
                None,
            )),
            "acme",
        ),
        (Expr::Delete(delete_with_input), "override"),
        (
            Expr::Invoke(InvokeExpr::with_target(
                "document_touch",
                target.clone(),
                Some(explicit),
            )),
            "override",
        ),
    ] {
        let mut mat = SessionMaterialization::new();
        mat.stamp_capability_params(
            &target,
            [("workspace".into(), Value::String("acme".into()))]
                .into_iter()
                .collect(),
        );
        let before = requests.lock().unwrap().len();
        let preflight =
            preflight_compile_expr(&expr, &cgs, &compiled, &ViewAmbientContext::default(), &mat);
        assert_eq!(
            requests.lock().unwrap().len(),
            before,
            "preflight must not send requests"
        );
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
        assert!(live.is_ok(), "live: {live:?}");
        let guard = requests.lock().unwrap();
        assert_eq!(guard.len(), before + 1);
        let (_, uri) = guard.last().unwrap();
        let query: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap().as_bytes())
                .into_owned()
                .collect();
        assert_eq!(query.get("workspace").map(String::as_str), Some(expected));
        assert_eq!(
            query.get("identity").map(String::as_str),
            Some("/notes/a.md")
        );
        drop(guard);
        assert!(preflight.is_ok(), "preflight: {preflight:?}");
    }
    for expr in [
        Expr::Delete(DeleteExpr::with_target("document_delete", target.clone())),
        Expr::Invoke(InvokeExpr::with_target(
            "document_touch",
            target.clone(),
            None,
        )),
    ] {
        let mut mat = SessionMaterialization::new();
        let before = requests.lock().unwrap().len();
        let preflight =
            preflight_compile_expr(&expr, &cgs, &compiled, &ViewAmbientContext::default(), &mat);
        assert!(
            matches!(&preflight, Err(plasm_runtime::RuntimeError::CmlError { source: plasm_compile::CmlError::VariableNotFound { name } }) if name == "workspace"),
            "{preflight:?}"
        );
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
            matches!(&live, Err(plasm_runtime::RuntimeError::CmlError { source: plasm_compile::CmlError::VariableNotFound { name } }) if name == "workspace"),
            "{live:?}"
        );
        assert_eq!(requests.lock().unwrap().len(), before);
    }
    server.abort();
}
