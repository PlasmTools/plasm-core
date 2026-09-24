//! Intentional integration coverage of the packaged AppWorld catalog contracts.
use plasm_core::{Expr, Value};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, SessionMaterialization, StreamConsumeOpts,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn gmail_message_preserves_sender_recipients_and_attachment_identity() {
    let row = serde_json::json!({"email_id":7,"subject":"Documents","body":"Attached",
        "created_at":"2026-01-02T03:04:05Z","response_to_email_id":null,
        "sender":{"name":"Alex","email":"alex@example.com"},
        "recipients":[{"name":"Sam","email":"sam@example.com"}],
        "attachments":[{"id":19,"file_name":"application.pdf"}]});
    let app = axum::Router::new().route(
        "/gmail/emails/7",
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
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/gmail"),
    )
    .unwrap();
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(url),
        ..Default::default()
    })
    .unwrap();
    for (program, field, expected) in [
        (r#"Email("7")"#, "sender_email", "alex@example.com"),
        (r#"Email("7").recipients"#, "email", "sam@example.com"),
        (r#"Email("7").attachments"#, "file_name", "application.pdf"),
    ] {
        let expr = plasm_core::expr_parser::parse_session_line(program, &cgs, None)
            .unwrap()
            .expr;
        let mut mat = SessionMaterialization::new();
        mat.stamp_capability_params(
            &plasm_core::Ref::new("Email", "7"),
            indexmap::IndexMap::from([("access_token".into(), Value::String("fixture".into()))]),
        );
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
        assert_eq!(result.entities.len(), 1, "{program}");
        assert_eq!(
            result.entities[0].fields[field].to_value(),
            Value::String(expected.into()),
            "{program}"
        );
        if field == "file_name" {
            assert_eq!(
                result.entities[0].fields["attachment_id"].to_value(),
                Value::Integer(19)
            );
        }
    }
    server.abort();
}

#[tokio::test]
async fn amazon_order_lines_keep_purchase_identity_and_product_reference() {
    let app = axum::Router::new().route(
        "/amazon/order/{id}",
        axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<i64>| async move {
                axum::Json(serde_json::json!({"order_id":id,"order_items":[{
                "product_id":9,"product_name":"Backpack","ordered_quantity":id,
                "returned_quantity":0,"gift_wrap_quantity":0,"price":12.5,
                "product_review_id":null,"expected_delivery_at":"2026-01-02T03:04:05Z",
                "delivered_at":null}]}))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/amazon"),
    )
    .unwrap();
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(url),
        ..Default::default()
    })
    .unwrap();
    let mut mat = SessionMaterialization::new();
    let mut refs = Vec::new();
    for id in [1, 2] {
        mat.stamp_capability_params(
            &plasm_core::Ref::new("Order", id.to_string()),
            indexmap::IndexMap::from([("access_token".into(), Value::String("fixture".into()))]),
        );
        let expr = plasm_core::expr_parser::parse_session_line(
            &format!("Order(\"{id}\").items"),
            &cgs,
            None,
        )
        .unwrap()
        .expr;
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
        assert_eq!(result.entities.len(), 1);
        let row = &result.entities[0];
        assert_eq!(row.fields["order_id"].to_value(), Value::Integer(id));
        assert_eq!(
            row.fields["ordered_quantity"].to_value(),
            Value::Integer(id)
        );
        assert_eq!(row.fields["product"].to_value(), Value::String("9".into()));
        refs.push(row.reference.clone());
    }
    assert_ne!(refs[0], refs[1]);
    server.abort();
}

#[tokio::test]
async fn gmail_typed_schedule_encodes_only_at_transport_boundary() {
    let app = axum::Router::new().route(
        "/gmail/drafts",
        axum::routing::post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                assert_eq!(body["scheduled_send_at"], "2030-01-02|01:04:05");
                axum::Json(serde_json::json!({"draft_id":7,"message":"Draft created."}))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/gmail"),
    )
    .unwrap();
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let input = Value::Object(indexmap::IndexMap::from([
        ("access_token".into(), Value::String("fixture".into())),
        (
            "recipient_email_addresses".into(),
            Value::Array(vec![Value::String("sam@example.com".into())]),
        ),
        ("subject".into(), Value::String("Contract".into())),
        ("body".into(), Value::String("Scheduled".into())),
        (
            "scheduled_send_at".into(),
            Value::String("2030-01-02T03:04:05+02:00".into()),
        ),
    ]));
    let expr = Expr::Create(plasm_core::CreateExpr::new("draft_create", "Draft", input));
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(url),
        ..Default::default()
    })
    .unwrap();
    let result = engine
        .execute(
            &expr,
            &cgs,
            &mut SessionMaterialization::new(),
            None,
            StreamConsumeOpts::default(),
            ExecuteOptions {
                compiled_catalog: Some(compiled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result.entities.len(), 1);
    server.abort();
}

/// Cart identity belongs to its request scope, never to its changing checkout total.
#[tokio::test]
async fn amazon_cart_preserves_request_identity_across_totals_and_codec() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let app = axum::Router::new().route(
        "/amazon/cart",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let seen = seen.clone();
            async move {
                assert!(matches!(
                    headers["authorization"].to_str().unwrap(),
                    "Bearer fixture" | "Bearer other"
                ));
                let total = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    23.4
                } else {
                    0.0
                };
                axum::Json(serde_json::json!({
                    "total_cost": total, "base_cost": 20.0, "delivery_fee": 2.0,
                    "tax": 1.4, "gift_wrap_fee": 0.0, "discount": 0.0,
                    "promo_code": null, "promo_valid": false,
                    "cart_items": [{"product_id": 7, "product_name": "Fixture product",
                        "price": 20.0, "quantity": 1, "delivery_days": 2, "gift_wrap_quantity": 0}]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let source = plasm_core::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/amazon"),
    )
    .unwrap();
    let cgs = serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let compiled = Arc::new(
        plasm_compile::CompiledCatalog::decode_artifact(
            &serde_json::to_vec(&compiled).unwrap(),
            &cgs,
        )
        .unwrap(),
    );
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(url),
        ..Default::default()
    })
    .unwrap();
    let mut identities = Vec::new();
    for (token, expected) in [("fixture", 23.4), ("fixture", 0.0), ("other", 0.0)] {
        let expr = plasm_core::expr_parser::parse_session_line(
            &format!(r#"Cart{{access_token="{token}"}}"#),
            &cgs,
            None,
        )
        .unwrap()
        .expr;
        let mut mat = SessionMaterialization::new();
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
        assert_eq!(result.entities.len(), 1);
        let row = &result.entities[0];
        assert_eq!(row.fields["total_cost"].to_value(), Value::Float(expected));
        identities.push(row.reference.clone());
    }
    assert_eq!(identities[0], identities[1]);
    assert_ne!(identities[0], identities[2]);
    assert_eq!(count.load(Ordering::SeqCst), 3);
    server.abort();
}

#[test]
fn gmail_and_amazon_deployment_requirements_are_closed() {
    use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, DeploymentBindings};
    use std::collections::{BTreeMap, BTreeSet};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld");
    let mut schemas = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.join("domain.yaml").is_file() {
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            let mut cgs = plasm_core::load_schema_dir(&path).unwrap();
            cgs.bind_registry_entry_id(&name);
            schemas.insert(name, cgs);
        }
    }
    let catalogs = schemas
        .iter()
        .map(|(name, cgs)| (name.clone(), cgs))
        .collect();
    let allowed = schemas.keys().cloned().collect::<BTreeSet<_>>();
    let selected = ["gmail", "amazon"]
        .into_iter()
        .flat_map(|name| {
            schemas[name]
                .capabilities
                .keys()
                .map(move |cap| CapabilityRef {
                    catalog: name.into(),
                    capability: cap.to_string(),
                })
        })
        .collect::<Vec<_>>();
    let bindings: DeploymentBindings =
        serde_json::from_slice(&std::fs::read(root.join("deployment-bindings.json")).unwrap())
            .unwrap();
    let closure = prerequisite_closure(&catalogs, &bindings, &selected, &allowed).unwrap();
    assert_eq!(closure.business.len(), selected.len());
    assert!(!closure.acquisitions.is_empty());
}
