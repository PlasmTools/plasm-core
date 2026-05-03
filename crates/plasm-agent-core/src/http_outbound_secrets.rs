//! Control-plane: store outbound API secrets in auth-framework `kv_store` (`hosted_kv` keys in CGS).

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::control_plane_http::internal_or_outbound_setup_authorized;
use crate::server_state::PlasmHostState;
#[derive(Debug, Deserialize)]
struct PutBody {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct DeleteBody {
    key: String,
}

async fn put_kv_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PutBody>,
) -> Result<StatusCode, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "outbound secret write") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(storage) = st.auth_storage() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let key = body.key.trim();
    if key.is_empty() || key.len() > 512 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let ok_prefix = key.starts_with("plasm:outbound:") || key.starts_with("plasm:oauth_app:v1:");
    if !ok_prefix {
        tracing::warn!(
            key = %key,
            "secret key rejected (must start with plasm:outbound: or plasm:oauth_app:v1:)"
        );
        return Err(StatusCode::BAD_REQUEST);
    }
    storage
        .store_kv(key, body.value.as_bytes(), None)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "outbound secret store_kv failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_kv_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DeleteBody>,
) -> Result<StatusCode, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "outbound secret write") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(storage) = st.auth_storage() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let key = body.key.trim();
    if key.is_empty() || key.len() > 512 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let ok_prefix = key.starts_with("plasm:outbound:") || key.starts_with("plasm:oauth_app:v1:");
    if !ok_prefix {
        tracing::warn!(
            key = %key,
            "secret key delete rejected (must start with plasm:outbound: or plasm:oauth_app:v1:)"
        );
        return Err(StatusCode::BAD_REQUEST);
    }
    storage.delete_kv(key).await.map_err(|e| {
        tracing::warn!(error = %e, "outbound secret delete_kv failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn outbound_secrets_routes() -> Router {
    Router::new()
        .route("/internal/outbound-secrets/v1/put", post(put_kv_handler))
        .route(
            "/internal/outbound-secrets/v1/delete",
            post(delete_kv_handler),
        )
}
