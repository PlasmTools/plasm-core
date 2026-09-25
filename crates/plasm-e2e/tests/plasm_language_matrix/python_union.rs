//! Tagged-input union laws, alongside the original rowset semantic matrix.
use plasm_agent::{execute_session::ExecuteSession, plasm_compile::compile_python_program};
use plasm_core::{CgsContext, TeachingExposureSession};
use std::sync::Arc;

// These features extend input typing; they do not certify unrelated native row obligations.
pub(crate) const FEATURES: &[&str] = &[
    "tagged_union_overloads",
    "nested_union_arrays",
    "union_variant_rejection",
    "union_wire_remapping",
    "union_optional_remapped_fields",
    "union_common_input_lanes",
];

fn fixture() -> (ExecuteSession, String, String, String, String) {
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_union_matrix"),
    )
    .unwrap();
    cgs.bind_registry_entry_id("langmatrix");
    let cgs = Arc::new(cgs);
    let exposure = TeachingExposureSession::new(&cgs, "langmatrix", &["Record"]);
    let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        &exposure,
        &Default::default(),
    )
    .unwrap();
    assert!(wave
        .capabilities
        .iter()
        .all(|cap| cap.unavailable.is_none()));
    let symbols = exposure.to_symbol_map();
    let entity = symbols.entity_sym_for("langmatrix", "Record");
    let write = symbols.method_sym_for("langmatrix", "Record", "record_write");
    let batch = symbols.method_sym_for("langmatrix", "Record", "record_batch");
    let contexts = indexmap::IndexMap::from([(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    )]);
    let session = ExecuteSession::new(
        "langmatrix".into(),
        String::new(),
        cgs.clone(),
        contexts,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        vec!["Record".into()],
        Some(exposure),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );
    (session, entity, write, batch, wave.declarations)
}

#[tokio::test]
async fn python_union_matrix_preserves_variants_and_rejects_mixtures() {
    let (session, entity, write, batch, declarations) = fixture();
    assert!(declarations.contains("@overload"));
    assert!(declarations.contains("Literal[\"text\"]"));
    assert!(declarations.contains("Literal[\"count\"]"));
    assert!(declarations.contains(" | "));
    for args in [
        "kind='text', text='hello'",
        "kind='text', text='hello', note='memo'",
        "kind='count', count=3, labels=['red', 'blue']",
    ] {
        let source = format!("class Change(Program):\n    def build(self):\n        return {entity}.get('r1').{write}(tenant='t1', request_id='req1', {args})");
        let bundle = compile_python_program(&session, &source)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        let dry = plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();
        assert!(!dry.node_results.is_empty());
    }
    let source = format!("class Change(Program):\n    def build(self):\n        return {entity}.get('r1').{batch}(operations=[{{'kind':'text','text':'hello'}},{{'kind':'count','count':3,'labels':['red']}}])");
    let bundle = compile_python_program(&session, &source).unwrap();
    plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();

    for args in [
        "kind='unknown', text='hello'",
        "text='hello'",
        "kind='text', text='hello', count=1",
        "kind='text'",
        "kind='text', text=''",
        "kind='text', text='ok', note=''",
        "kind='count', count='three', labels=['red']",
        "kind='count', count=-1, labels=['red']",
        "kind='count', count=1, labels=['green']",
    ] {
        let source = format!("class Change(Program):\n    def build(self):\n        return {entity}.get('r1').{write}(tenant='t1', request_id='req1', {args})");
        assert!(
            compile_python_program(&session, &source).is_err(),
            "accepted invalid union: {args}"
        );
    }
    for operations in [
        "[{'kind':'text','text':'hello','labels':['red']}]",
        "[{'kind':'count','count':2,'labels':['green']}]",
        "[{'kind':'text','text':'ok'},{'kind':'count','count':2}]",
    ] {
        let source = format!("class Change(Program):\n    def build(self):\n        return {entity}.get('r1').{batch}(operations={operations})");
        assert!(
            compile_python_program(&session, &source).is_err(),
            "accepted invalid nested union: {operations}"
        );
    }
}

#[tokio::test]
async fn python_union_matrix_live_wire_preserves_nested_variant_shapes() {
    use axum::{routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::sync::Mutex;
    let writes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = writes.clone();
    let app = Router::new().route(
        "/records/{id}",
        get(|| async { Json(json!({"id":"r1"})) }).patch(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(json!({"id":"r1"}))
            }
        }),
    );
    let captured = writes.clone();
    let app = app.route(
        "/records/{id}/batch",
        axum::routing::post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(json!({"id":"r1"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (session, entity, write, batch, _) = fixture();
    let host = super::super::language_matrix::matrix_host_state(
        plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        session.cgs.clone(),
    );
    for (method, args) in [
        (&write, "tenant='t1', request_id='req1', kind='text', text='hello'"),
        (&write, "tenant='t1', request_id='req1', kind='text', text='hello', note='memo'"),
        (&write, "tenant='t1', request_id='req1', kind='count', count=3, labels=['red','blue']"),
        (&batch, "operations=[{'kind':'text','text':'nested'},{'kind':'count','count':2,'labels':['blue']}]"),
    ] {
        let source = format!("class Change(Program):\n    def build(self):\n        return {entity}.get('r1').{method}({args})");
        let bundle = compile_python_program(&session, &source).unwrap();
        plasm_agent::plasm_plan_run::run_plasm_comp(
            &session, &host, &session.prompt_hash, "union-live", &bundle, true,
            None, None, None, None,
        ).await.unwrap();
    }
    server.abort();
    assert_eq!(
        *writes.lock().unwrap(),
        vec![
            json!({"tenant":"t1","request_id":"req1","kind":"text","content":{"text":"hello"}}),
            json!({"tenant":"t1","request_id":"req1","kind":"text","content":{"text":"hello","note":"memo"}}),
            json!({"tenant":"t1","request_id":"req1","kind":"count","count":3,"labels":["red","blue"]}),
            json!({"operations":[{"kind":"text","content":{"text":"nested"}},{"kind":"count","count":2,"labels":["blue"]}]}),
        ]
    );
}
