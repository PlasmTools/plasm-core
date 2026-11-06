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

struct NextUrlPages;

struct RepeatingUrlPages(AtomicUsize);

#[async_trait]
impl HttpTransport for RepeatingUrlPages {
    async fn send_compiled_http(
        &self,
        _: &str,
        _: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        Ok((
            serde_json::json!({"results":[{"id":"first","n":1}], "next":"http://fixture/page-2"}),
            None,
        ))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok((
            serde_json::json!({"results":[{"id":format!("next-{n}"),"n":n}], "next":"http://fixture/page-2"}),
            None,
        ))
    }
}

struct LinkPage;

#[async_trait]
impl HttpTransport for LinkPage {
    async fn send_compiled_http(
        &self,
        _: &str,
        _: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        Ok((
            serde_json::json!({"results":[{"id":"first","n":1}]}),
            Some("http://fixture/page-2".to_owned()),
        ))
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
async fn response_cache_preserves_next_link() {
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        Arc::new(LinkPage),
        None,
    );
    let request = CompiledOperation::Http(CompiledRequest {
        credential: None,
        method: plasm_compile::HttpMethod::Get,
        path: "/items".into(),
        query: None,
        body: None,
        body_format: Default::default(),
        multipart: None,
        headers: None,
    });
    let mut mat = SessionMaterialization::new();
    let first = engine
        .execute_with_replay_full(&request, ExecutionMode::Live, Some(&mut mat))
        .await
        .unwrap();
    let cached = engine
        .execute_with_replay_full(&request, ExecutionMode::Live, Some(&mut mat))
        .await
        .unwrap();
    assert_eq!(first.1.as_deref(), Some("http://fixture/page-2"));
    assert_eq!(
        cached.1, first.1,
        "a cached page must retain its continuation"
    );
}

#[tokio::test]
async fn replay_preserves_next_link() {
    use crate::ReplayStore;
    let mut engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        Arc::new(LinkPage),
        None,
    );
    let request = CompiledOperation::Http(CompiledRequest {
        credential: None,
        method: plasm_compile::HttpMethod::Get,
        path: "/items".into(),
        query: None,
        body: None,
        body_format: Default::default(),
        multipart: None,
        headers: None,
    });
    let fingerprint = crate::RequestFingerprint::from_operation(&request).unwrap();
    engine
        .replay_store
        .as_mut()
        .unwrap()
        .store(
            fingerprint,
            crate::ReplayEntry {
                request: request.clone(),
                outcome: crate::replay::TransportOutcome {
                    response: serde_json::json!({"results":[{"id":"first","n":1}]}),
                    next_link: Some("http://fixture/page-2".into()),
                },
                decoded_entities: vec![],
                schema_snapshot: serde_json::json!({}),
                timestamp: 0,
            },
        )
        .unwrap();
    let (_, next, source) = engine
        .execute_with_replay_full(&request, ExecutionMode::Replay, None)
        .await
        .unwrap();
    assert_eq!(source, ExecutionSource::Replay);
    assert_eq!(next.as_deref(), Some("http://fixture/page-2"));
}

#[tokio::test]
async fn replay_query_refuses_unrecorded_absolute_continuation() {
    use super::super::pagination_driver::{PageRequest, PaginationDriver};
    use crate::ReplayStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let next_url = format!("{base_url}/page-2");
    let link = next_url.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        socket.read(&mut buf).await.unwrap();
        let body = r#"{"results":[{"id":"first","n":1}]}"#;
        let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLink: <{}>; rel=\"next\"\r\nConnection: close\r\n\r\n{}", body.len(), link, body);
        socket.write_all(reply.as_bytes()).await.unwrap();
    });
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    cgs.capabilities
        .get_mut("item_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["pagination"] = serde_json::json!({
        "strategy":"link_header", "location":"link_header", "params":{}
    });
    let catalog = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let template = catalog.capability("item_query").unwrap();
    let base = plasm_compile::compile_operation(template, &plasm_compile::CmlEnv::new()).unwrap();
    let config =
        plasm_compile::pagination_config_for_capability(cgs.get_capability("item_query").unwrap())
            .unwrap();
    let mut driver = PaginationDriver::try_from_config(
        config,
        &plasm_core::QueryPagination::default(),
        &StreamConsumeOpts::default(),
    )
    .unwrap();
    let PageRequest::Compiled(request) = driver.prepare_request(&base).unwrap() else {
        panic!("first page is compiled")
    };
    let request = *request;
    let fingerprint = crate::RequestFingerprint::from_operation(&request).unwrap();
    let mut engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url),
        ..Default::default()
    })
    .unwrap();
    let (body, next_link, _) = engine
        .execute_with_replay_full(&request, ExecutionMode::Live, None)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(next_link.as_deref(), Some(next_url.as_str()));
    engine
        .replay_store
        .as_mut()
        .unwrap()
        .store(
            fingerprint,
            crate::ReplayEntry {
                request,
                outcome: crate::replay::TransportOutcome {
                    response: body,
                    next_link,
                },
                decoded_entities: vec![],
                schema_snapshot: serde_json::json!({}),
                timestamp: 0,
            },
        )
        .unwrap();
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut SessionMaterialization::new(),
            Some(ExecutionMode::Replay),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await;
    assert!(
        matches!(result, Err(RuntimeError::LiveAbsolutePaginationRequired)),
        "must not claim cached first page is exhaustive: {result:?}"
    );
}

#[tokio::test]
async fn replay_query_returns_all_recorded_pages() {
    use super::super::pagination_driver::{PageObservation, PageRequest, PaginationDriver};
    use crate::ReplayStore;
    let cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    let catalog = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let base = plasm_compile::compile_operation(
        catalog.capability("item_query").unwrap(),
        &plasm_compile::CmlEnv::new(),
    )
    .unwrap();
    let config =
        plasm_compile::pagination_config_for_capability(cgs.get_capability("item_query").unwrap())
            .unwrap();
    let mut driver = PaginationDriver::try_from_config(
        config,
        &plasm_core::QueryPagination::default(),
        &StreamConsumeOpts::default(),
    )
    .unwrap();
    let mut engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        Arc::new(LinkPage),
        None,
    );
    for (ordinal, range) in [0..20, 20..21].into_iter().enumerate() {
        let PageRequest::Compiled(request) = driver.prepare_request(&base).unwrap() else {
            panic!("parameter pagination")
        };
        let request = *request;
        let rows: Vec<_> = range
            .map(|n| serde_json::json!({"id":format!("id-{n}"),"n":n}))
            .collect();
        let ids: Vec<_> = rows
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect();
        let response = serde_json::json!({"results":rows});
        driver
            .observe_page(PageObservation {
                ordinal: ordinal as u32,
                entity_ids: &ids,
                returned_rows: ids.len() as u32,
                accepted_rows: ids.len() as u32,
                response: &response,
                next_link: None,
            })
            .unwrap();
        let fingerprint = crate::RequestFingerprint::from_operation(&request).unwrap();
        engine
            .replay_store
            .as_mut()
            .unwrap()
            .store(
                fingerprint,
                crate::ReplayEntry {
                    request,
                    outcome: crate::replay::TransportOutcome {
                        response,
                        next_link: None,
                    },
                    decoded_entities: vec![],
                    schema_snapshot: serde_json::json!({}),
                    timestamp: 0,
                },
            )
            .unwrap();
    }
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut SessionMaterialization::new(),
            Some(ExecutionMode::Replay),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.count(), 21);
    assert_eq!(result.coverage(), ResultCoverage::Complete);
    assert_eq!(result.source, ExecutionSource::Replay);
}

#[async_trait]
impl HttpTransport for NextUrlPages {
    async fn send_compiled_http(
        &self,
        _: &str,
        _: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        Ok((
            serde_json::json!({"results": [{"id":"first","n":1}], "next":"http://fixture/page-2"}),
            None,
        ))
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        assert_eq!(url, "http://fixture/page-2");
        Ok((
            serde_json::json!({"results": [{"id":"second","n":2}], "next":null}),
            None,
        ))
    }
}

#[tokio::test]
async fn next_url_distinct_pages_do_not_repeat_coordinate() {
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    cgs.capabilities
        .get_mut("item_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["pagination"] = serde_json::json!({
        "strategy":"next_url", "location":"response_next_url",
        "response_next_url_field":"next", "params":{"limit":{"fixed":20,"role":"page_size"}}
    });
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        Arc::new(NextUrlPages),
        None,
    );
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut SessionMaterialization::new(),
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .expect("distinct pages must advance");
    assert_eq!(result.count(), 2);
    assert_eq!(result.coverage(), ResultCoverage::Complete);
}

#[tokio::test]
async fn next_url_repeated_coordinate_is_rejected() {
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    cgs.capabilities
        .get_mut("item_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["pagination"] = serde_json::json!({
        "strategy":"next_url", "location":"response_next_url", "response_next_url_field":"next", "params":{"limit":{"fixed":20,"role":"page_size"}}
    });
    let transport = Arc::new(RepeatingUrlPages(AtomicUsize::new(0)));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            ..Default::default()
        },
        transport.clone(),
        None,
    );
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut SessionMaterialization::new(),
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await;
    assert!(matches!(
        result,
        Err(RuntimeError::PaginationProgress {
            reason: PaginationTerminalReason::RepeatedCoordinate
        })
    ));
    assert_eq!(
        transport.0.load(Ordering::SeqCst),
        1,
        "reject the repeated URL before issuing it again"
    );
}

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

struct PrecisionCrossPages;

#[async_trait]
impl HttpTransport for PrecisionCrossPages {
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let body = match request.path.as_str() {
            "/language/v1/tags" => serde_json::json!([{"id":"t1","item_id":"i1","label":"tag"}]),
            "/language/v1/items/i1" => {
                serde_json::json!({"id":"i1","title":"selected","score":9007199254740993_i64,"owner":"chosen"})
            }
            path => panic!("unexpected path {path}"),
        };
        Ok((body, None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected request")
    }
}

#[tokio::test]
async fn pull_right_preserves_exact_integer_comparison() {
    let mut cgs = load_schema_dir(
        &std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    cgs.capabilities
        .get_mut("langtag_query")
        .unwrap()
        .inputs
        .scope
        .0
        .clear();
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture".into()),
            hydrate: false,
            ..Default::default()
        },
        Arc::new(PrecisionCrossPages),
        None,
    );
    for (field, value) in [
        ("score", Value::Integer(9007199254740992)),
        ("title", Value::String("alpha".into())),
    ] {
        let mut mat = SessionMaterialization::new();
        let query = QueryExpr::filtered(
            "LangTag",
            Predicate::Comparison {
                field: format!("item_id.{field}"),
                op: plasm_core::CompOp::Gt,
                value: value.clone().try_into().unwrap(),
            },
        );
        let crosses = [plasm_core::cross_entity::CrossEntityPredicate {
            ref_field: "item_id".into(),
            foreign_entity: "LangItem".into(),
            foreign_predicate: Predicate::Comparison {
                field: field.into(),
                op: plasm_core::CompOp::Gt,
                value: value.try_into().unwrap(),
            },
        }];
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
            engine.execute_query_cross_entity(
                &query,
                &crosses,
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
    }
}

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
