use crate::{handlers, MockStore};
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// Create the HTTP router for the mock server
pub fn create_router(store: Arc<RwLock<MockStore>>) -> Router {
    Router::new()
        // Health and info endpoints
        .route("/health", get(handlers::health_check))
        .route("/info", get(handlers::server_info))
        // Resource query endpoints
        .route("/query/{entity_type}", post(handlers::query_resources))
        // Resource CRUD endpoints
        .route("/resources/{entity_type}/{id}", get(handlers::get_resource))
        .route(
            "/resources/{entity_type}/{id}/{relation}",
            get(handlers::get_related_resources),
        )
        // Add shared state
        .with_state(store)
        // Add middleware
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(
                    CorsLayer::new()
                        .allow_origin(Any)
                        .allow_methods(Any)
                        .allow_headers(Any),
                ),
        )
}

/// Start the mock server
pub async fn start_server(store: MockStore, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(port = port, "Starting mock server");

    let shared_store = Arc::new(RwLock::new(store));
    let app = create_router(shared_store);

    let listener = tokio::net::TcpListener::bind(&format!("0.0.0.0:{port}")).await?;

    tracing::info!(port = port, bind = "0.0.0.0", "Mock server listening");

    axum::serve(listener, app).await?;

    Ok(())
}

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub enable_cors: bool,
    pub enable_tracing: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 3000,
            enable_cors: true,
            enable_tracing: true,
        }
    }
}

/// Start the server with custom configuration
pub async fn start_server_with_config(
    store: MockStore,
    config: ServerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if config.enable_tracing {
        tracing_subscriber::fmt::init();
    }

    start_server(store, config.port).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{
        FieldSchema, FieldType, FieldValueKind, NamedValueSchema, ResourceSchema, StringSemantics,
        ValueDomainKey, CGS,
    };

    fn create_test_store() -> MockStore {
        let mut schema = CGS::new();
        for k in ["mock_srv_str_id", "mock_srv_str_name"] {
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
                        ValueDomainKey::new("mock_srv_str_id").expect("key"),
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
                        ValueDomainKey::new("mock_srv_str_name").expect("key"),
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

        MockStore::new(schema)
    }

    #[test]
    fn test_create_router() {
        let store = create_test_store();
        let shared_store = Arc::new(RwLock::new(store));
        let _router = create_router(shared_store);
        // Just test that it doesn't panic
    }

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 3000);
        assert!(config.enable_cors);
        assert!(config.enable_tracing);
    }
}
