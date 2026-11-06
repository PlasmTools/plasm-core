//! Query projection and embedded observations through real HTTP execution.
use plasm_core::{Expr, QueryExpr, Ref, Value};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};

async fn serve(body: serde_json::Value) -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().fallback(axum::routing::get(move || {
        let body = body.clone();
        async move { axum::Json(body) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}

fn pagination() -> serde_json::Value {
    serde_json::json!({"strategy":"page_number","params":{"page_index":{"counter":0},"page_limit":{"fixed":20,"role":"page_size"}}})
}

#[tokio::test]
async fn numeric_root_projection_matches_single_and_paginated_queries() {
    let (base, server) = serve(serde_json::json!([{"results":[{"id":"a","n":7}]}])).await;
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    let mapping = &mut cgs
        .capabilities
        .get_mut("item_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0;
    mapping["response"] = serde_json::json!({"items_path":["0","results"]});
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut query = QueryExpr::all("Item");
    query.capability_name = Some("item_query".into());
    for paginated in [false, true] {
        let mapping = &mut cgs
            .capabilities
            .get_mut("item_query")
            .unwrap()
            .mapping
            .as_mut()
            .unwrap()
            .template
            .0;
        if paginated {
            mapping["pagination"] = pagination();
        } else {
            mapping.as_object_mut().unwrap().remove("pagination");
        }
        let result = engine
            .execute(
                &Expr::Query(query.clone()),
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
            .expect("root array query");
        assert_eq!(result.count(), 1, "paginated={paginated}");
        assert_eq!(result.entities()[0].reference.primary_slot_str(), "a");
    }
    server.abort();
}

#[tokio::test]
async fn paginated_query_publishes_embedded_children_without_hydration() {
    let (base, server) = serve(
        serde_json::json!([{"id":"i1", "lines":[{"id":"l1","item_id":"i1","note":"observed"}]}]),
    )
    .await;
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix_views"),
    )
    .unwrap();
    cgs.capabilities
        .retain(|_, cap| cap.domain.as_str() != "LangLine");
    cgs.entities.get_mut("LangLine").unwrap().abstract_entity = true;
    cgs.capabilities
        .get_mut("langitem_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["pagination"] = pagination();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut query = QueryExpr::all("LangItem");
    query.capability_name = Some("langitem_query".into());
    let mut mat = SessionMaterialization::new();
    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.count(), 1);
    let child = mat
        .graph
        .get(&Ref::new("LangLine", "l1"))
        .expect("embedded child is retained");
    assert_eq!(
        child.fields["note"].to_value(),
        Value::String("observed".into())
    );
    assert_eq!(
        child.completeness,
        plasm_runtime::EntityCompleteness::Summary
    );
    server.abort();
}

#[tokio::test]
async fn bounded_queries_retain_only_selected_observation_trees() {
    let (base, server) = serve(serde_json::json!([
        {"id":"i1","title":"one","score":1,"lines":[{"id":"l1","item_id":"i1","note":"one"}]},
        {"id":"i2","title":"two","score":9,"lines":[{"id":"l2","item_id":"i2","note":"two"}]}
    ]))
    .await;
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix_views"),
    )
    .unwrap();
    cgs.capabilities
        .retain(|_, cap| cap.domain.as_str() != "LangLine");
    cgs.entities.get_mut("LangLine").unwrap().abstract_entity = true;
    cgs.capabilities
        .get_mut("langitem_query")
        .unwrap()
        .mapping
        .as_mut()
        .unwrap()
        .template
        .0["pagination"] = pagination();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let mut query = QueryExpr::all("LangItem");
    query.capability_name = Some("langitem_query".into());
    for (consume, expected, excluded) in [
        (
            StreamConsumeOpts {
                fetch_all: true,
                max_items: Some(1),
                bound_kind: plasm_runtime::ConsumeBoundKind::ExpressionTake,
                ..Default::default()
            },
            ("i1", "l1"),
            ("i2", "l2"),
        ),
        (
            StreamConsumeOpts {
                fetch_all: true,
                top_k: Some(plasm_runtime::TopKSpec {
                    count: 1,
                    sort_key: vec!["score".into()],
                    descending: true,
                    row_filter: vec![],
                }),
                ..Default::default()
            },
            ("i2", "l2"),
            ("i1", "l1"),
        ),
    ] {
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Query(query.clone()),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                consume,
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.count(), 1);
        assert!(mat.graph.get(&Ref::new("LangItem", expected.0)).is_some());
        assert!(mat.graph.get(&Ref::new("LangLine", expected.1)).is_some());
        assert!(mat.graph.get(&Ref::new("LangItem", excluded.0)).is_none());
        assert!(mat.graph.get(&Ref::new("LangLine", excluded.1)).is_none());
    }
    server.abort();
}

#[tokio::test]
async fn later_detail_observation_is_not_overwritten_by_list_embed() {
    let app = axum::Router::new()
        .route(
            "/language/v1/items",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!([
                    {"id":"i1","lines":[{"id":"l1","item_id":"i1","note":"list"}]}
                ]))
            }),
        )
        .route(
            "/language/v1/items/i1",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "id":"i1","title":"detail","lines":[{"id":"l1","item_id":"i1","note":"detail"}]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix_views"),
    )
    .unwrap();
    cgs.capabilities
        .retain(|_, cap| cap.domain.as_str() != "LangLine");
    cgs.entities.get_mut("LangLine").unwrap().abstract_entity = true;
    let cap = cgs.capabilities.get_mut("langitem_query").unwrap();
    cap.provides.retain(|f| f.as_str() == "id");
    cap.mapping.as_mut().unwrap().template.0["pagination"] = pagination();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        ..Default::default()
    })
    .unwrap();
    let mut mat = SessionMaterialization::new();
    mat.poison_read_caches_after_mutation();
    let mut query = QueryExpr::all("LangItem");
    query.capability_name = Some("langitem_query".into());
    engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        mat.graph.get(&Ref::new("LangLine", "l1")).unwrap().fields["note"].to_value(),
        Value::String("detail".into())
    );
    server.abort();
}

#[tokio::test]
async fn top_k_hydration_does_not_publish_discarded_descendants() {
    let app = axum::Router::new()
        .route("/language/v1/items", axum::routing::get(|| async { axum::Json(serde_json::json!([
            {"id":"i1","score":1}, {"id":"i2","score":9}
        ])) }))
        .route("/language/v1/items/{id}", axum::routing::get(|axum::extract::Path(id): axum::extract::Path<String>| async move {
            let (score, line) = if id == "i1" { (1, "l1") } else { (9, "l2") };
            axum::Json(serde_json::json!({"id":id,"title":"detail","score":score,"lines":[{"id":line,"item_id":id,"note":"detail"}]}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(plasm_e2e::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix_views"),
    )
    .unwrap();
    cgs.capabilities
        .retain(|_, cap| cap.domain.as_str() != "LangLine");
    cgs.entities.get_mut("LangLine").unwrap().abstract_entity = true;
    let cap = cgs.capabilities.get_mut("langitem_query").unwrap();
    cap.provides
        .retain(|f| matches!(f.as_str(), "id" | "score"));
    cap.mapping.as_mut().unwrap().template.0["pagination"] = pagination();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        ..Default::default()
    })
    .unwrap();
    let mut query = QueryExpr::all("LangItem");
    query.capability_name = Some("langitem_query".into());
    let mut mat = SessionMaterialization::new();
    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                top_k: Some(plasm_runtime::TopKSpec {
                    count: 1,
                    sort_key: vec!["score".into()],
                    descending: true,
                    row_filter: vec![],
                }),
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.count(), 1);
    assert!(mat.graph.get(&Ref::new("LangLine", "l2")).is_some());
    assert!(mat.graph.get(&Ref::new("LangLine", "l1")).is_none());
    assert!(mat.graph.get(&Ref::new("LangItem", "i1")).is_none());
    server.abort();
}
