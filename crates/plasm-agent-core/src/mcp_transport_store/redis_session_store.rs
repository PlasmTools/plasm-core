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
