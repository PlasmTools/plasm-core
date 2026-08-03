use async_trait::async_trait;
use indexmap::IndexMap;
use plasm_compile::CompiledRequest;
use plasm_core::{Expr, InvokeExpr, Ref, Value, CGS};
use plasm_runtime::auth::ResolvedAuth;
use plasm_runtime::http_transport::HttpTransport;
use plasm_runtime::{
    preflight_compile_expr, CachedEntity, EntityCompleteness, ExecuteOptions, ExecutionConfig,
    ExecutionEngine, ExecutionMode, RuntimeError, SessionMaterialization, StreamConsumeOpts,
    ViewAmbientContext,
};
use std::sync::{Arc, Mutex};

fn fixture() -> CGS {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/hydrate_invoke_target");
    plasm_core::load_schema(&dir).expect("load hydrate_invoke_target fixture")
}

fn run_expr(capability: &str) -> Expr {
    let mut input = indexmap::indexmap! {
        "expr".to_string() => Value::String("up".to_string()),
    };
    if capability == "datasource_run" {
        input.insert(
            "from".to_string(),
            Value::String("2026-08-03T03:50:00Z".to_string()),
        );
        input.insert(
            "to".to_string(),
            Value::String("2026-08-03T04:00:00Z".to_string()),
        );
    }
    Expr::Invoke(InvokeExpr::new(
        capability,
        "Datasource",
        "prometheus",
        Some(Value::Object(input)),
    ))
}

#[test]
fn static_compile_hydrates_declared_entity_fields_and_honors_prefix() {
    let cgs = fixture();
    let ambient = ViewAmbientContext::default();

    preflight_compile_expr(&run_expr("datasource_run"), &cgs, &ambient)
        .expect("ds_type comes from Datasource fields even though provides omits it");
    preflight_compile_expr(&run_expr("datasource_run_source_prefix"), &cgs, &ambient)
        .expect("source_type should honor the configured prefix");

    let error = preflight_compile_expr(&run_expr("datasource_run_typo"), &cgs, &ambient)
        .expect_err("ds_typo must remain an unknown CML variable");
    assert!(error.to_string().contains("ds_typo"), "{error}");
}

#[test]
fn grafana_datasource_actions_compile_with_unchanged_ds_type_mapping() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/grafana");
    let cgs = plasm_core::load_schema(&dir).expect("load Grafana catalog");
    let ambient = ViewAmbientContext::default();
    let cases = [
        (
            "datasource_query_run",
            indexmap::indexmap! {
                "expr".to_string() => Value::String("up".to_string()),
            },
        ),
        (
            "datasource_clickhouse_query_run",
            indexmap::indexmap! {
                "query".to_string() => Value::String("SELECT 1".to_string()),
            },
        ),
        (
            "panel_query_run",
            indexmap::indexmap! {
                "expr".to_string() => Value::String("up".to_string()),
            },
        ),
    ];

    for (capability, input) in cases {
        let expr = Expr::Invoke(InvokeExpr::new(
            capability,
            "Datasource",
            "prometheus",
            Some(Value::Object(input)),
        ));
        preflight_compile_expr(&expr, &cgs, &ambient)
            .unwrap_or_else(|error| panic!("{capability} failed static compile: {error}"));
    }
}

#[derive(Clone, Default)]
struct RecordingTransport {
    requests: Arc<Mutex<Vec<CompiledRequest>>>,
}

#[async_trait]
impl HttpTransport for RecordingTransport {
    async fn send_compiled_http(
        &self,
        _base_url: &str,
        request: &CompiledRequest,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.path.ends_with("/api/datasources/uid/prometheus") {
            return Ok((
                serde_json::json!({"uid": "prometheus", "type": "prometheus"}),
                None,
            ));
        }
        if request.path.ends_with("/api/ds/query") {
            return Ok((
                serde_json::json!({
                    "uid": "prometheus",
                    "query_results": {"status": "success"}
                }),
                None,
            ));
        }
        panic!("unexpected request: {request:?}");
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET: {url}");
    }
}

async fn execute(cgs: &CGS, cached: Option<EntityCompleteness>) -> Vec<CompiledRequest> {
    let transport = RecordingTransport::default();
    let requests = transport.requests.clone();
    let engine =
        ExecutionEngine::new_with_transport(ExecutionConfig::default(), Arc::new(transport), None);
    let mut cache = SessionMaterialization::new();
    if let Some(completeness) = cached {
        cache
            .insert(CachedEntity::from_decoded(
                Ref::new("Datasource", "prometheus"),
                indexmap::indexmap! {
                    "uid".to_string() => Value::String("prometheus".to_string()),
                    "type".to_string() => Value::String("prometheus".to_string()),
                },
                IndexMap::new(),
                1,
                completeness,
            ))
            .expect("seed cache");
    }

    engine
        .execute(
            &run_expr("datasource_run"),
            cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .expect("execute hydrated action");
    let captured = requests.lock().unwrap().clone();
    captured
}

fn action_body(requests: &[CompiledRequest]) -> serde_json::Value {
    let request = requests
        .iter()
        .find(|request| request.path.ends_with("/api/ds/query"))
        .expect("outbound action request");
    serde_json::to_value(request.body.as_ref().expect("action body")).expect("serialize body")
}

#[tokio::test]
async fn live_hydration_dispatches_real_value_for_fresh_complete_and_incomplete_cache_paths() {
    let cgs = fixture();
    let fresh = execute(&cgs, None).await;
    let complete = execute(&cgs, Some(EntityCompleteness::Complete)).await;
    let incomplete = execute(&cgs, Some(EntityCompleteness::Summary)).await;

    assert_eq!(fresh.len(), 2, "fresh path must GET then POST");
    assert_eq!(complete.len(), 1, "complete cache must skip GET");
    assert_eq!(incomplete.len(), 2, "incomplete cache must GET then POST");

    let expected = serde_json::json!({
        "queries": [{
            "datasource": {"type": "prometheus", "uid": "prometheus"},
            "expr": "up"
        }],
        "from": "2026-08-03T03:50:00Z",
        "to": "2026-08-03T04:00:00Z"
    });
    for requests in [&fresh, &complete, &incomplete] {
        let body = action_body(requests);
        assert_eq!(body, expected);
        assert!(!body.to_string().contains("preflight-stub"));
    }
}
