//! Public compiler → dry review → run regression for deferred delete operands.

#[path = "common/language_matrix.rs"]
#[allow(dead_code)]
mod language_matrix;

use axum::{
    extract::State,
    http::{HeaderMap, Method, Uri},
    Json, Router,
};
use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_core::PromptPipelineConfig;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use std::future::IntoFuture;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedRequest {
    method: Method,
    path: String,
    authorization: Option<String>,
}

async fn receive(
    State(requests): State<Arc<Mutex<Vec<ObservedRequest>>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    requests.lock().unwrap().push(ObservedRequest {
        method,
        path: uri.path().to_owned(),
        authorization: headers
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned()),
    });
    Json(serde_json::json!({}))
}

#[test]
fn reviewed_numeric_fanout_delete_preserves_required_inputs() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(reviewed_delete_case());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn reviewed_delete_case() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(receive).with_state(requests.clone()),
        )
        .into_future(),
    );
    let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
    cgs.http_backend = base.clone();
    let input_contract = cgs.capabilities["langitem_secured_touch"].clone();
    let delete = cgs.capabilities.get_mut("langitem_delete").unwrap();
    delete.inputs = input_contract.inputs;
    delete.mapping = input_contract.mapping;
    delete.mapping.as_mut().unwrap().template.0["method"] = serde_json::json!("DELETE");
    let cgs = Arc::new(cgs);
    let session = language_matrix::matrix_execute_session(cgs.clone());
    let host = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    let program = r#"rows = [{id:42, access_token:"fixture-a"}, {id:43, access_token:"fixture-b"}]
done = rows => LangItem(_.id).delete(access_token=_.access_token)
done"#;
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "typed-delete-closure",
        program,
    )
    .expect("compile");
    evaluate_plasm_comp_dry(&session, &bundle).expect("review");
    assert!(
        requests.lock().unwrap().is_empty(),
        "review performed network acquisition"
    );
    run_plasm_comp(
        &session,
        &host,
        &session.prompt_hash,
        "typed-delete",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("reviewed program must run");
    let observed = requests.lock().unwrap().clone();
    assert_eq!(observed.len(), 2);
    for (request, id, token) in [
        (&observed[0], 42, "fixture-a"),
        (&observed[1], 43, "fixture-b"),
    ] {
        assert_eq!(request.method, Method::DELETE);
        assert!(request.path.contains(&format!("/{id}/")), "{request:?}");
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Bearer {token}").as_str())
        );
    }
    for source in ["rows = []", "rows = [{id:42}]"] {
        let missing = format!("{source}\ndone = rows => LangItem(_.id).delete()\ndone");
        let rejection = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "missing-delete-input",
            &missing,
        )
        .and_then(|bundle| evaluate_plasm_comp_dry(&session, &bundle).map(|_| bundle));
        let error =
            rejection.expect_err("missing required input must fail even for an empty source");
        assert!(
            error.to_string().contains("access_token"),
            "unexpected rejection: {error}"
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
    server.abort();
}
