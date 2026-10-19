//! Durable protocol handshake state. rmcp owns restoration and local runtime lifetime.

use super::RedisBackend;
use rmcp::transport::streamable_http_server::session::store::{
    SessionState, SessionStore, SessionStoreError,
};
use std::sync::Arc;

/// A distinct record namespace prevents interpreting old SDK runtime records as rmcp state.
const PREFIX: &str = "mcp:rmcp-session:";

pub struct RedisMcpSessionStore {
    backend: Arc<RedisBackend>,
}

impl RedisMcpSessionStore {
    pub fn new(backend: Arc<RedisBackend>) -> Self {
        Self { backend }
    }
    pub(crate) async fn touch(&self, id: &str) -> redis::RedisResult<()> {
        self.backend.touch_strict(&format!("{PREFIX}{id}")).await
    }
}

#[async_trait::async_trait]
impl SessionStore for RedisMcpSessionStore {
    async fn load(&self, id: &str) -> Result<Option<SessionState>, SessionStoreError> {
        let key = format!("{PREFIX}{id}");
        let Some(payload) = self.backend.get_string_strict(&key).await? else {
            return Ok(None);
        };
        let state = serde_json::from_str(&payload)?;
        self.backend.touch_strict(&key).await?;
        Ok(Some(state))
    }

    async fn store(&self, id: &str, state: &SessionState) -> Result<(), SessionStoreError> {
        let payload = serde_json::to_string(state)?;
        self.backend
            .set_string_strict(&format!("{PREFIX}{id}"), &payload)
            .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), SessionStoreError> {
        self.backend.delete_strict(&format!("{PREFIX}{id}")).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        Router,
    };
    use http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    async fn post(
        app: Router,
        session: Option<&str>,
        message: Value,
    ) -> (http::HeaderMap, StatusCode, Value) {
        let mut request = Request::post("/mcp")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25");
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        let response = app
            .oneshot(request.body(Body::from(message.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let value = if text.is_empty() {
            Value::Null
        } else if text.starts_with('{') {
            serde_json::from_str(text).unwrap()
        } else if status.is_success() {
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter(|data| !data.trim().is_empty())
                .map(|data| serde_json::from_str::<Value>(data).unwrap())
                .find(|data| data.get("id") == message.get("id"))
                .expect("response event")
        } else {
            Value::String(text.to_owned())
        };
        (headers, status, value)
    }

    async fn router(backend: Arc<RedisBackend>) -> Router {
        let mut host = (*crate::test_support::operation_fixtures::minimal_host()).clone();
        host.oss.redis_backend = Some(backend);
        crate::mcp_server::build_mcp_router_for_transport(
            Arc::new(host),
            crate::mcp_server::McpHttpTransport::StreamableHttp,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires isolated PLASM_TEST_REDIS_URL"]
    async fn rmcp_restores_handshake_across_instances_and_deletes_durable_state() {
        let url = std::env::var("PLASM_TEST_REDIS_URL").expect("isolated Redis URL");
        let backend = Arc::new(
            RedisBackend::connect(&url, std::time::Duration::from_secs(120))
                .await
                .unwrap(),
        );
        let first = router(backend.clone()).await;
        let (headers, status, init) = post(first.clone(), None, json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25",
                "clientInfo":{"name":"restored-client","version":"1"},
                "capabilities":{"extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]}}}
            }
        })).await;
        assert_eq!(status, StatusCode::OK, "{init}");
        let id = headers["mcp-session-id"].to_str().unwrap();
        let (_, status, _) = post(
            first,
            Some(id),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let store = RedisMcpSessionStore::new(backend.clone());
        assert_eq!(
            store
                .load(id)
                .await
                .unwrap()
                .unwrap()
                .initialize_params
                .client_info
                .name,
            "restored-client"
        );
        let second = router(backend.clone()).await;
        let (_, status, tools) = post(
            second.clone(),
            Some(id),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tools}");
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "plasm_ui_read_plan"),
            "restored client capabilities must govern UI exposure"
        );
        let invalid = second
            .clone()
            .oneshot(
                Request::post("/mcp")
                    .header("authorization", "Bearer invalid")
                    .header("mcp-session-id", id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            invalid.status(),
            StatusCode::UNAUTHORIZED,
            "restoration must not restore credentials"
        );
        let response = second
            .oneshot(
                Request::delete("/mcp")
                    .header("host", "localhost")
                    .header("mcp-session-id", id)
                    .header("mcp-protocol-version", "2025-11-25")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert!(store.load(id).await.unwrap().is_none());
        let third = router(backend).await;
        let (_, status, _) = post(
            third,
            Some(id),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[ignore = "requires isolated PLASM_TEST_REDIS_URL"]
    async fn rmcp_session_store_distinguishes_corrupt_records_from_absence() {
        let backend = Arc::new(
            RedisBackend::connect(
                &std::env::var("PLASM_TEST_REDIS_URL").unwrap(),
                std::time::Duration::from_secs(120),
            )
            .await
            .unwrap(),
        );
        let store = RedisMcpSessionStore::new(backend.clone());
        let id = uuid::Uuid::new_v4().to_string();
        assert!(store.load(&id).await.unwrap().is_none());
        backend
            .set_string_strict(&format!("{PREFIX}{id}"), "{broken")
            .await
            .unwrap();
        let error = store.load(&id).await.unwrap_err();
        assert!(error.is::<serde_json::Error>());
        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }
}
