//! Pagination must preserve the same field-coverage contract as a single response.
use super::*;
use crate::auth::ResolvedAuth;
use crate::http_transport::HttpTransport;
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::loader::load_schema_dir;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
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
                return Err(RuntimeError::CacheError {
                    message: "detail unavailable".into(),
                });
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
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
