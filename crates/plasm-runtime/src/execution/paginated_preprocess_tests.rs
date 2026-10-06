//! Native pagination metadata and preprocessed semantic fields are independent.
use super::*;
use crate::auth::ResolvedAuth;
use async_trait::async_trait;
use plasm_core::TypedFieldValue;
use std::sync::Mutex;

#[derive(Default)]
struct MembershipPages {
    cursors: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl HttpTransport for MembershipPages {
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        assert_eq!(request.path.trim_start_matches('/'), "memberships");
        let cursor = match &request.query {
            Some(Value::Object(query)) => query.get("cursor").map(|value| match value {
                Value::String(cursor) => cursor.clone(),
                other => panic!("unexpected cursor: {other:?}"),
            }),
            other => panic!("missing pagination query: {other:?}"),
        };
        let mut cursors = self.cursors.lock().unwrap();
        cursors.push(cursor.clone());
        let body = match cursors.len() {
            1 => {
                assert_eq!(cursor, None);
                serde_json::json!({"members": ["user-a"], "response_metadata": {"next_cursor": "page-b"}})
            }
            2 => {
                assert_eq!(cursor.as_deref(), Some("page-b"));
                serde_json::json!({"members": ["user-b"], "response_metadata": {}})
            }
            _ => panic!("pagination did not stop after the native cursor was exhausted"),
        };
        Ok((body, None))
    }

    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("membership query must not require hydration or absolute requests")
    }
}

#[tokio::test]
async fn paginated_preprocess_preserves_membership_foreign_keys_and_native_cursor() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/paginated_preprocess"),
    )
    .unwrap();
    let membership = cgs.get_entity("Membership").unwrap();
    let user = membership
        .fields
        .get("user")
        .unwrap()
        .named_value(&cgs)
        .unwrap();
    assert!(
        matches!(&user.field_type, FieldType::EntityRef { target, .. } if target.as_str() == "User")
    );
    let transport = Arc::new(MembershipPages::default());
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://fixture.invalid".into()),
            ..Default::default()
        },
        transport.clone(),
        None,
    );
    let mut mat = SessionMaterialization::new();
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Membership")),
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
    assert_eq!(result.coverage(), ResultCoverage::Complete);
    assert_eq!(result.count(), 2);
    for (row, id) in result.entities().iter().zip(["user-a", "user-b"]) {
        assert_eq!(row.reference, Ref::new("Membership", id));
        assert_eq!(
            row.fields.get("id"),
            Some(&TypedFieldValue::String(id.into()))
        );
        assert_eq!(
            row.fields.get("user"),
            Some(&TypedFieldValue::String(id.into())),
            "preprocessing must decode the foreign-key field, not only synthesize the row identity"
        );
        assert_eq!(row.completeness, EntityCompleteness::Complete);
        let cached = mat.get(&row.reference).unwrap();
        assert_eq!(cached.fields.get("user"), row.fields.get("user"));
    }
    assert_eq!(
        *transport.cursors.lock().unwrap(),
        vec![None, Some("page-b".into())]
    );
}
