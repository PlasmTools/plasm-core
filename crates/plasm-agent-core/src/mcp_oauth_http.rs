//! Axum adapter for the same OAuth provider used by the SDK transport.
use crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use http::{Request, StatusCode};
use rust_mcp_sdk::auth::AuthProvider;
use std::sync::Arc;

pub(crate) fn router(provider: Arc<PlasmMcpApiKeyAuthProvider>) -> Router {
    let mut router = Router::new();
    for path in provider
        .auth_endpoints()
        .into_iter()
        .flat_map(|endpoints| endpoints.keys())
    {
        router = router.route(path, any(handle));
    }
    router
        .with_state(provider)
        .layer(DefaultBodyLimit::max(65536))
}

async fn handle(
    State(provider): State<Arc<PlasmMcpApiKeyAuthProvider>>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 65536).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "OAuth body exceeds 64 KiB").into_response()
        }
    };
    let body = match std::str::from_utf8(&bytes) {
        Ok(body) => body,
        Err(_) => return (StatusCode::BAD_REQUEST, "OAuth body must be UTF-8").into_response(),
    };
    match provider
        .handle_oauth_http(Request::from_parts(parts, body))
        .await
    {
        Ok(response) => response.map(Body::new),
        Err(error) => {
            tracing::warn!(error = %error, "OAuth HTTP adapter failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "OAuth service unavailable",
            )
                .into_response()
        }
    }
}
