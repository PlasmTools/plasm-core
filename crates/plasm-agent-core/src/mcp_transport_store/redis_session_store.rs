//! Redis-backed `rust-mcp-sdk` [`SessionStore`] with cross-pod hydration.

use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::server_runtime::create_server_instance;
use rust_mcp_sdk::mcp_server::ServerRuntime;
use rust_mcp_sdk::schema::{
    ClientMessage, InitializeRequestParams, InitializeResult, ServerMessage,
};
use rust_mcp_sdk::session_store::{InMemorySessionStore, SessionStore};
use rust_mcp_sdk::task_store::{ClientTaskStore, ServerTaskStore};
use rust_mcp_sdk::SessionId;
use rust_mcp_sdk::{McpObserver, McpServer, McpServerHandler};
use tracing::warn;

use super::redis_backend::RedisBackend;

const TRANSPORT_KEY_PREFIX: &str = "mcp:transport:";

#[derive(Clone)]
pub struct SessionRuntimeFactory {
    pub server_details: Arc<InitializeResult>,
    pub handler: Arc<dyn McpServerHandler>,
    pub task_store: Option<Arc<ServerTaskStore>>,
    pub client_task_store: Option<Arc<ClientTaskStore>>,
    pub message_observer: Option<Arc<dyn McpObserver<ClientMessage, ServerMessage>>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TransportSessionRecord {
    init_payload: Option<String>,
}

/// Local cache + Redis metadata; hydrates [`ServerRuntime`] on any pod when metadata exists.
pub struct RedisSessionStore {
    local: InMemorySessionStore,
    backend: Arc<RedisBackend>,
    factory: Arc<SessionRuntimeFactory>,
}

impl RedisSessionStore {
    pub fn new(backend: Arc<RedisBackend>, factory: Arc<SessionRuntimeFactory>) -> Self {
        Self {
            local: InMemorySessionStore::new(),
            backend,
            factory,
        }
    }

    pub async fn ping(&self) -> redis::RedisResult<()> {
        self.backend.ping().await
    }

    fn transport_key(session_id: &SessionId) -> String {
        format!("{TRANSPORT_KEY_PREFIX}{session_id}")
    }

    async fn load_record(&self, session_id: &SessionId) -> Option<TransportSessionRecord> {
        self.backend
            .get_json(&Self::transport_key(session_id))
            .await
    }

    async fn write_record(&self, session_id: &SessionId, record: &TransportSessionRecord) {
        self.backend
            .set_json(&Self::transport_key(session_id), record)
            .await;
    }

    async fn touch_record(&self, session_id: &SessionId) {
        self.backend.touch(&Self::transport_key(session_id)).await;
    }

    async fn delete_record(&self, session_id: &SessionId) {
        self.backend.delete(&Self::transport_key(session_id)).await;
    }

    async fn hydrate_runtime(&self, session_id: &SessionId) -> Option<Arc<ServerRuntime>> {
        let record = self.load_record(session_id).await?;
        let runtime = create_server_instance(
            Arc::clone(&self.factory.server_details),
            Arc::clone(&self.factory.handler),
            session_id.to_owned(),
            None,
            self.factory.task_store.clone(),
            self.factory.client_task_store.clone(),
            self.factory.message_observer.clone(),
        );
        if let Some(payload) = record.init_payload.as_deref() {
            if let Some(params) = parse_initialize_params(payload) {
                if let Err(err) = runtime.set_client_details(params).await {
                    warn!(?err, %session_id, "hydrated runtime set_client_details failed");
                }
            }
        }
        self.local.set(session_id.to_owned(), runtime.clone()).await;
        Some(runtime)
    }
}

fn parse_initialize_params(payload: &str) -> Option<InitializeRequestParams> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("method")?.as_str()? != "initialize" {
        return None;
    }
    serde_json::from_value(value.get("params")?.clone()).ok()
}

#[async_trait]
impl SessionStore for RedisSessionStore {
    async fn get(&self, key: &SessionId) -> Option<Arc<ServerRuntime>> {
        if let Some(runtime) = self.local.get(key).await {
            return Some(runtime);
        }
        self.hydrate_runtime(key).await
    }

    async fn set(&self, key: SessionId, value: Arc<ServerRuntime>) {
        self.local.set(key, value).await;
    }

    async fn delete(&self, key: &SessionId) {
        self.local.delete(key).await;
    }

    async fn delete_persistent(&self, key: &SessionId) {
        self.local.delete(key).await;
        self.delete_record(key).await;
    }

    async fn has(&self, session: &SessionId) -> bool {
        self.local.has(session).await || self.load_record(session).await.is_some()
    }

    async fn keys(&self) -> Vec<SessionId> {
        self.local.keys().await
    }

    async fn values(&self) -> Vec<Arc<ServerRuntime>> {
        self.local.values().await
    }

    async fn clear(&self) {
        self.local.clear().await;
    }

    async fn persist_session_metadata(&self, key: &SessionId, init_payload: Option<&str>) {
        self.write_record(
            key,
            &TransportSessionRecord {
                init_payload: init_payload.map(str::to_string),
            },
        )
        .await;
    }

    async fn touch_session(&self, key: &SessionId) {
        self.touch_record(key).await;
    }

    async fn exists_remote(&self, key: &SessionId) -> bool {
        self.load_record(key).await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::mcp_server::PlasmMcpHandler;
    use crate::run_artifacts::RunArtifactStore;
    use crate::server_state::CatalogBootstrap;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use rust_mcp_sdk::schema::{Implementation, InitializeResult};
    use rust_mcp_sdk::ToMcpServerHandler;

    fn test_host_state() -> crate::server_state::PlasmHostState {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = std::sync::Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "langmatrix".into(),
            "Lang Matrix".into(),
            vec!["matrix".into()],
            cgs,
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: std::sync::Arc::new(reg),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: std::sync::Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    #[tokio::test]
    async fn delete_local_only_preserves_remote_record() {
        let url = match std::env::var("PLASM_TEST_REDIS_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let backend = std::sync::Arc::new(
            super::super::redis_backend::RedisBackend::connect(&url, Duration::from_secs(120))
                .await
                .expect("redis"),
        );
        let plasm = std::sync::Arc::new(test_host_state());
        let handler = PlasmMcpHandler::new(std::sync::Arc::clone(&plasm)).to_mcp_server_handler();
        let factory = std::sync::Arc::new(SessionRuntimeFactory {
            server_details: std::sync::Arc::new(InitializeResult {
                protocol_version: "2025-11-25".into(),
                capabilities: Default::default(),
                server_info: Implementation {
                    name: "test".into(),
                    version: "0".into(),
                    title: None,
                    description: None,
                    icons: vec![],
                    website_url: None,
                },
                instructions: None,
                meta: None,
            }),
            handler,
            task_store: None,
            client_task_store: None,
            message_observer: None,
        });
        let store = RedisSessionStore::new(backend, factory);
        let session_id: SessionId = format!("test-delete-local-only-{}", uuid::Uuid::new_v4());

        store
            .persist_session_metadata(&session_id, Some(r#"{"method":"initialize","params":{}}"#))
            .await;
        assert!(store.exists_remote(&session_id).await);

        store.delete(&session_id).await;
        assert!(store.exists_remote(&session_id).await);

        store.delete_persistent(&session_id).await;
        assert!(!store.exists_remote(&session_id).await);
    }
}
