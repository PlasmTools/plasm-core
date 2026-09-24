//! Relation-only rows are observable without inventing a detail capability.
use plasm_core::Value;
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, SessionMaterialization, StreamConsumeOpts,
};
use std::sync::Arc;

#[tokio::test]
async fn embedded_relation_without_get_preserves_rows_and_summary_status() {
    let mut cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views"),
    )
    .unwrap();
    cgs.capabilities
        .retain(|_, cap| cap.domain.as_str() != "LangLine");
    cgs.entities.get_mut("LangLine").unwrap().abstract_entity = true;
    cgs.capabilities
        .get_mut("langitem_query")
        .unwrap()
        .provides
        .retain(|field| field.as_str() == "id");
    // The catalogue serialization boundary must preserve the absence of a Get.
    let cgs = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
    let row = serde_json::json!({"id":"i1", "lines":[
        {"id":"l2","item_id":"i1","note":"second"},
        {"id":"l1","item_id":"i1","note":"first"},
        {"id":"l2","item_id":"i1","note":"second"}]});
    let app = axum::Router::new()
        .route(
            "/language/v1/items",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!([{"id":"i1","title":"summary"}]))
            }),
        )
        .route(
            "/language/v1/items/i1",
            axum::routing::get(move || {
                let row = row.clone();
                async move { axum::Json(row) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(url),
        ..Default::default()
    })
    .unwrap();
    for (program, seed_parent, requests) in [
        (r#"LangItem("i1").lines"#, false, 1),
        (r#"LangItem("i1").self_via_bindings.lines"#, true, 1),
        ("LangItem{}.lines", false, 2),
    ] {
        let expr = plasm_core::expr_parser::parse_session_line(program, &cgs, None)
            .unwrap()
            .expr;
        let mut mat = SessionMaterialization::new();
        if seed_parent {
            mat.insert(plasm_runtime::CachedEntity::from_decoded(
                plasm_core::Ref::new("LangItem", "i1"),
                indexmap::IndexMap::from([("id".into(), Value::String("i1".into()))]),
                Default::default(),
                0,
                plasm_runtime::EntityCompleteness::Complete,
            ))
            .unwrap();
        }

        let result = engine
            .execute(
                &expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions {
                    compiled_catalog: Some(compiled.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result
                .entities
                .iter()
                .map(|row| row.fields["note"].to_value())
                .collect::<Vec<_>>(),
            vec![
                Value::String("second".into()),
                Value::String("first".into()),
                Value::String("second".into())
            ],
            "{program}"
        );
        assert!(result
            .entities
            .iter()
            .all(|row| row.completeness == plasm_runtime::EntityCompleteness::Summary));
        assert_eq!(result.stats.network_requests, requests, "{program}");
    }
    server.abort();
}
