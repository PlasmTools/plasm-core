use crate::{MockError, MockStore};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    Json as RequestJson,
};
use plasm_compile::BackendFilter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type SharedStore = Arc<RwLock<MockStore>>;

/// Request body for query operations
#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    #[serde(default)]
    pub filter: Option<BackendFilter>,
    #[serde(default)]
    pub projection: Option<Vec<String>>,
}

/// Response format for query operations
#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub results: Vec<Value>,
    pub count: usize,
}

/// Response format for single resource operations  
#[derive(Debug, Serialize)]
pub struct ResourceResponse {
    pub resource: Value,
}

/// Error response format
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

/// Query resources of a given type
pub async fn query_resources(
    State(store): State<SharedStore>,
    Path(entity_type): Path<String>,
    request: Option<RequestJson<QueryRequest>>,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let query_req = request.map(|r| r.0).unwrap_or(QueryRequest {
        filter: None,
        projection: None,
    });

    let store = store.read().await;

    let resources = store
        .query_resources(&entity_type, query_req.filter.as_ref())
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;

    let results: Vec<Value> = resources
        .into_iter()
        .map(|r| resource_to_json(&r, &query_req.projection))
        .collect();

    Ok(Json(QueryResponse {
        count: results.len(),
        results,
    }))
}

/// Get a single resource by ID
pub async fn get_resource(
    State(store): State<SharedStore>,
    Path((entity_type, id)): Path<(String, String)>,
) -> Result<Json<ResourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let store = store.read().await;

    let resource = store
        .get_resource(&entity_type, &id)
        .map_err(|e| error_response(StatusCode::NOT_FOUND, e))?;

    Ok(Json(ResourceResponse {
        resource: resource_to_json(resource, &None),
    }))
}

/// Get related resources
pub async fn get_related_resources(
    State(store): State<SharedStore>,
    Path((entity_type, id, relation)): Path<(String, String, String)>,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let store = store.read().await;

    let resources = store
        .get_related_resources(&entity_type, &id, &relation)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;

    let results: Vec<Value> = resources
        .into_iter()
        .map(|r| resource_to_json(&r, &None))
        .collect();

    Ok(Json(QueryResponse {
        count: results.len(),
        results,
    }))
}

/// Health check endpoint
pub async fn health_check() -> &'static str {
    "OK"
}

/// Get server information
pub async fn server_info(State(store): State<SharedStore>) -> Json<Value> {
    let store = store.read().await;
    let entities: Vec<String> = store
        .schema
        .entities
        .keys()
        .map(|k| k.to_string())
        .collect();

    Json(serde_json::json!({
        "service": "plasm-mock",
        "version": "0.1.0",
        "entities": entities,
        "capabilities": store
            .schema
            .capabilities
            .keys()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
    }))
}

/// Convert a mock resource to JSON for API response
fn resource_to_json(resource: &crate::MockResource, projection: &Option<Vec<String>>) -> Value {
    let mut json = serde_json::json!({
        "id": resource.id
    });

    // Add fields
    for (field_name, field_value) in &resource.fields {
        if let Some(proj) = projection {
            if !proj.contains(field_name) {
                continue;
            }
        }
        json[field_name] = serde_json::to_value(field_value).unwrap_or(Value::Null);
    }

    // Add relations as references
    if projection.is_none()
        || projection
            .as_ref()
            .unwrap()
            .iter()
            .any(|p| resource.relations.contains_key(p))
    {
        for (relation_name, related_ids) in &resource.relations {
            if let Some(proj) = projection {
                if !proj.contains(relation_name) {
                    continue;
                }
            }
            json[relation_name] = Value::Array(
                related_ids
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            );
        }
    }

    json
}

/// Create an error response
fn error_response(status: StatusCode, error: MockError) -> (StatusCode, Json<ErrorResponse>) {
    let error_type = match error {
        MockError::EntityNotFound { .. } => "EntityNotFound",
        MockError::ResourceNotFound { .. } => "ResourceNotFound",
        MockError::FilterError { .. } => "FilterError",
        MockError::InvalidRequest { .. } => "InvalidRequest",
        MockError::SerializationError { .. } => "SerializationError",
        MockError::ConfigurationError { .. } => "ConfigurationError",
    };

    (
        status,
        Json(ErrorResponse {
            error: error_type.to_string(),
            message: error.to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request},
        Router,
    };
    use plasm_core::{
        FieldSchema, FieldType, FieldValueKind, NamedValueSchema, ResourceSchema, StringSemantics,
        ValueDomainKey,
    };
    use tower::ServiceExt;

    async fn create_test_app() -> (Router, SharedStore) {
        let mut schema = plasm_core::CGS::new();
        for k in ["mock_h_str_id", "mock_h_str_name"] {
            schema.values.insert(
                k.into(),
                NamedValueSchema {
                    description: String::new(),
                    field_type: FieldType::String,
                    value_format: None,
                    allowed_values: None,
                    string_semantics: Some(StringSemantics::Short),
                    array_items: None,
                },
            );
        }
        let account = ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_h_str_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_h_str_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        };
        schema.add_resource(account).unwrap();

        let mut store = MockStore::new(schema);
        let resource = crate::MockResource::new("test-1").with_field("name", "Test Account");
        store.add_resource("Account", resource).unwrap();

        let shared_store = Arc::new(RwLock::new(store));
        let app = crate::create_router(shared_store.clone());

        (app, shared_store)
    }

    #[tokio::test]
    async fn test_query_resources() {
        let (app, _store) = create_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/query/Account")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["results"][0]["id"], "test-1");
        assert_eq!(parsed["results"][0]["name"], "Test Account");
    }

    #[tokio::test]
    async fn test_get_resource() {
        let (app, _store) = create_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/resources/Account/test-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["resource"]["id"], "test-1");
        assert_eq!(parsed["resource"]["name"], "Test Account");
    }

    #[tokio::test]
    async fn test_health_check() {
        let (app, _store) = create_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, "OK");
    }
}
