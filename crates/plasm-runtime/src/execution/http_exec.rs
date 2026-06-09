//! HTTP transport helpers (compiled request + absolute URL GET).

use super::*;

impl ExecutionEngine {
    /// Execute request and capture `Link` header (`rel="next"`) when present.
    pub(crate) async fn execute_http_request_full(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let base_url = self.effective_http_base_for_request();
        let auth = self.resolve_auth_http().await?;
        self.transport
            .send_compiled_http(base_url.as_ref(), request, auth)
            .await
    }

    /// GET absolute URL (used for `link_header` continuation pages).
    pub(crate) async fn get_json_absolute(
        &self,
        url: &str,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let auth = self.resolve_auth_http().await?;
        self.transport.get_json_absolute(url, auth).await
    }
}
