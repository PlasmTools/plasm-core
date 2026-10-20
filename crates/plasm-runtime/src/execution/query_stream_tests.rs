//! Pagination must preserve the same field-coverage contract as a single response.
use super::*;
use crate::auth::ResolvedAuth;
use crate::http_transport::HttpTransport;
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::loader::load_schema_dir;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct Pages {
    lists: AtomicUsize,
    details: AtomicUsize,
    summary: bool,
    missing: bool,
    unavailable: bool,
}
#[async_trait]
impl HttpTransport for Pages {
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        if let Some(id) = request.path.strip_prefix("/items/indexed/") {
            self.details.fetch_add(1, Ordering::SeqCst);
            if self.unavailable {
                return Err(crate::CacheError::DetailUnavailable.into());
            }
            return Ok((serde_json::json!({"id": id, "n": 42}), None));
        }
        let page = self.lists.fetch_add(1, Ordering::SeqCst);
        let range = match page {
            0 => 0..20,
            1 => 20..21,
            _ => 0..0,
        };
        let rows: Vec<_> = range
            .map(|i| {
                if self.summary || (self.missing && i == 0) {
                    serde_json::json!({"id": format!("id-{i}")})
                } else {
                    serde_json::json!({"id": format!("id-{i}"), "n": 42})
                }
            })
            .collect();
        Ok((serde_json::json!({"results": rows}), None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute request")
    }
}

async fn run_case(summary: bool, missing: bool, stale: bool, unavailable: bool) {
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    cgs.capabilities.get_mut("item_query").unwrap().provides = if summary {
        vec!["id".into()]
    } else {
        vec!["id".into(), "n".into()]
    };
    let transport = Arc::new(Pages {
        lists: AtomicUsize::new(0),
        details: AtomicUsize::new(0),
        summary,
        missing,
        unavailable,
    });
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        transport.clone(),
        None,
    );
    let mut mat = SessionMaterialization::new();
    if stale {
        mat.insert(CachedEntity::from_decoded(
            plasm_core::Ref::new("Item", "id-0"),
            [
                ("id".into(), Value::String("id-0".into())),
                ("n".into(), Value::Integer(-1)),
            ]
            .into_iter()
            .collect(),
            Default::default(),
            0,
            EntityCompleteness::Complete,
        ))
        .unwrap();
        mat.poison_read_caches_after_mutation();
    }
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await;
    let result = result.unwrap();
    assert_eq!(result.count(), 21);
    assert_eq!(transport.lists.load(Ordering::SeqCst), 2);
    assert_eq!(
        transport.details.load(Ordering::SeqCst),
        if summary {
            21
        } else if missing {
            1
        } else {
            0
        },
        "hydrate only genuine summary rows"
    );
    assert_eq!(
        result.coverage(),
        ResultCoverage::Complete,
        "field failure cannot remove observed members"
    );
    if unavailable {
        assert!(result
            .entities()
            .iter()
            .all(|e| e.has_unavailable_detail_fields()));
        assert!(result
            .entities()
            .iter()
            .all(|e| !e.fields.contains_key("n")));
        assert!(result
            .entities()
            .iter()
            .all(|e| e.unavailable_fields.contains("n")));
        return;
    }
    assert!(result
        .entities()
        .iter()
        .all(|e| e.completeness == EntityCompleteness::Complete));
    assert!(result
        .entities()
        .iter()
        .all(|e| e.fields.get("n") == Some(&plasm_core::TypedFieldValue::Integer(42))));
}
#[tokio::test]
async fn paginated_complete_rows_do_not_hydrate() {
    run_case(false, false, false, false).await;
}
#[tokio::test]
async fn paginated_summaries_still_hydrate() {
    run_case(true, false, false, false).await;
}
#[tokio::test]
async fn paginated_complete_rows_replace_stale_observations() {
    run_case(false, false, true, false).await;
}
#[tokio::test]
async fn paginated_missing_field_still_hydrates() {
    run_case(false, true, false, false).await;
}

#[tokio::test]
async fn exhausted_membership_survives_unavailable_hydration_fields() {
    run_case(true, false, false, true).await;
}

#[test]
fn terminal_query_rejects_undecomposed_cross_entity_predicate() {
    let cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig::default()).unwrap();
    let mut mat = SessionMaterialization::new();
    let query = QueryExpr::filtered("LangTag", Predicate::eq("item_id.title", "selected"));
    let result = engine.local_query_to_stream(
        &query,
        &cgs,
        &mut mat,
        ExecutionMode::Live,
        StreamConsumeOpts::default(),
        None,
        &ViewAmbientContext::default(),
    );
    assert!(
        matches!(result, Err(RuntimeError::NonlocalQueryAtTerminal { entity }) if entity == "LangTag")
    );
}

struct CrossQueryPages(Mutex<Vec<String>>);
#[async_trait]
impl HttpTransport for CrossQueryPages {
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.0.lock().unwrap().push(request.path.clone());
        let rows = match request.path.as_str() {
            "/language/v1/items" => {
                serde_json::json!([{"id":"i1","title":"selected","score":1,"owner":"chosen"}])
            }
            "/language/v1/tags" => serde_json::json!([{"id":"t1","item_id":"i1","label":"tag"}]),
            other => panic!("unexpected terminal query path {other}"),
        };
        Ok((rows, None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute request")
    }
}

#[tokio::test]
async fn cross_entity_query_executes_two_terminal_queries() {
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let cap = cgs.capabilities.get_mut("langtag_query").unwrap();
    let mut field = cap.inputs.scope.0.remove(0);
    field.selection_effect = Some(plasm_core::SelectionEffect::Filter);
    cap.inputs.selection.0.push(field);
    let transport = Arc::new(CrossQueryPages(Mutex::new(Vec::new())));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            hydrate: false,
            ..Default::default()
        },
        transport.clone(),
        None,
    );
    let mut mat = SessionMaterialization::new();
    // Exercise the internal cross-query decomposition seam with its normal
    // compiled-catalog task scope; authored Python admission is matrix-owned.
    let query = QueryExpr::filtered("LangTag", Predicate::eq("item_id.owner", "chosen"));
    let result = ExecutionEngine::run_in_execute_task_scopes(
        "http://fixture".into(),
        None,
        None,
        None,
        None,
        ExecuteOptions::for_catalog(&cgs)
            .unwrap()
            .compiled_catalog
            .unwrap(),
        None,
        None,
        engine.execute_query(
            &query,
            &cgs,
            &mut mat,
            ExecutionMode::Live,
            StreamConsumeOpts::default(),
            &ViewAmbientContext::default(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.entities().len(), 1);
    assert_eq!(result.entities()[0].reference.primary_slot_str(), "t1");
    assert_eq!(
        *transport.0.lock().unwrap(),
        vec!["/language/v1/items", "/language/v1/tags"]
    );
}
