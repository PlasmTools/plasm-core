//! Tenant MCP configuration HTTP API (`POST /internal/mcp-config/v1/*`, `/internal/mcp-api-key/v1/*`).
//!
//! Mounted from OSS HTTP when [`super::server_state::PlasmHostState::mcp_config_repository`] is set,
//! and from the hosted router (`plasm-saas`). Authenticated with [`super::control_plane_http`].

use auth_framework::errors::AuthError;
use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tracing::Instrument;
use uuid::Uuid;

use crate::control_plane_http::control_plane_headers_authorized;
use crate::mcp_api_key_registry::McpApiKeyListItem;
use crate::mcp_config_repository::McpConfigRepositoryError;
use crate::mcp_runtime_config::{McpConfigUpsertJson, McpRuntimeConfig};
use crate::server_state::PlasmHostState;

async fn upsert_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<McpConfigUpsertJson>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        crate::metrics::record_audit_control_plane("mcp.config.upsert", "denied");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        crate::metrics::record_audit_control_plane("mcp.config.upsert", "dependency_error");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let optional = body.auth_optional_ids_clean();
    let ws = body.workspace_slug_resolved().to_string();
    let ps = body.project_slug_resolved().to_string();
    let nm = body.name_resolved().to_string();
    let st_status = body.status_normalized().to_string();
    let cfg: McpRuntimeConfig = McpRuntimeConfig::try_from(body).map_err(|e: String| {
        crate::metrics::record_audit_control_plane("mcp.config.upsert", "validation_error");
        tracing::warn!(message = %e, "mcp config upsert parse");
        StatusCode::BAD_REQUEST
    })?;
    let config_id = cfg.id;
    let span =
        crate::spans::security_mcp_config_upsert(&cfg.id, cfg.tenant_id.as_str(), cfg.version);
    span.in_scope(|| {
        tracing::debug!(
            config_id = %cfg.id,
            tenant_id = %cfg.tenant_id,
            version = cfg.version,
            "MCP config upsert"
        );
    });
    repo.upsert_full(
        cfg,
        ws.as_str(),
        ps.as_str(),
        nm.as_str(),
        st_status.as_str(),
        &optional,
    )
    .await
    .map_err(|e| {
        tracing::warn!(message = %e, "mcp config upsert persist");
        match e {
            McpConfigRepositoryError::InvalidInput(_) => {
                crate::metrics::record_audit_control_plane("mcp.config.upsert", "validation_error");
                StatusCode::BAD_REQUEST
            }
            _ => {
                crate::metrics::record_audit_control_plane("mcp.config.upsert", "dependency_error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    })?;
    if st_status == "disabled" {
        if let Some(mcp_auth) = st.mcp_transport_auth() {
            let _ = mcp_auth.revoke_for_config(config_id).await;
        }
    }
    crate::metrics::record_audit_control_plane("mcp.config.upsert", "success");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
    #[serde(default)]
    space_type: Option<String>,
    #[serde(default)]
    owner_subject: Option<String>,
}

async fn list_configs_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let v = repo
        .list_configs_by_scope_json(
            &q.tenant_id,
            &q.workspace_slug,
            &q.project_slug,
            q.space_type.as_deref(),
            q.owner_subject.as_deref(),
        )
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp config list");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(v))
}

async fn get_config_detail_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let v = repo.get_config_detail_json(&id).await.map_err(|e| {
        tracing::warn!(message = %e, "mcp config get");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let Some(v) = v else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(v))
}

#[derive(Debug, Deserialize)]
struct ProvisionBody {
    config_id: uuid::Uuid,
    /// Required display name (non-empty after trim; max 128 scalars in registry).
    label: String,
}

fn require_mcp_key_name(s: String) -> Result<String, StatusCode> {
    let t = s.trim();
    if t.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(t.chars().take(128).collect())
}

fn map_mcp_key_err(e: AuthError) -> StatusCode {
    match e {
        AuthError::UserNotFound => StatusCode::NOT_FOUND,
        AuthError::InvalidInput(_) | AuthError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Debug, Serialize)]
struct ProvisionResponse {
    api_key: String,
    key_id: Uuid,
}

async fn provision_api_key_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ProvisionBody>,
) -> Result<Json<ProvisionResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        crate::metrics::record_audit_control_plane("mcp.api_key.provision", "denied");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        crate::metrics::record_audit_control_plane("mcp.api_key.provision", "dependency_error");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo.config_exists(&body.config_id).await.map_err(|_| {
        crate::metrics::record_audit_control_plane("mcp.api_key.provision", "dependency_error");
        StatusCode::INTERNAL_SERVER_ERROR
    })? {
        crate::metrics::record_audit_control_plane("mcp.api_key.provision", "not_found");
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        crate::metrics::record_audit_control_plane("mcp.api_key.provision", "dependency_error");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let label = require_mcp_key_name(body.label)?;
    let p = mcp_auth
        .provision_api_key(body.config_id, label)
        .instrument(crate::spans::security_mcp_api_key_provision(
            &body.config_id,
        ))
        .await
        .map_err(|e| {
            crate::metrics::record_audit_control_plane("mcp.api_key.provision", "dependency_error");
            tracing::warn!(message = %e, "mcp api key provision");
            map_mcp_key_err(e)
        })?;
    if let Ok(Some(status)) = mcp_auth.public_key_status(body.config_id).await {
        let _ = repo
            .set_mcp_api_key_fingerprint(body.config_id, Some(status.key_fingerprint.as_str()))
            .await;
    }
    crate::metrics::record_audit_control_plane("mcp.api_key.provision", "success");
    Ok(Json(ProvisionResponse {
        api_key: p.api_key,
        key_id: p.key_id,
    }))
}

#[derive(Debug, Deserialize)]
struct RotateBody {
    config_id: uuid::Uuid,
    /// Name for the single new key after all existing keys are revoked.
    label: String,
}

#[derive(Debug, Deserialize)]
struct ApiKeyStatusQuery {
    config_id: uuid::Uuid,
}

#[derive(Debug, serde::Serialize)]
struct ApiKeyStatusResponse {
    bound: bool,
    config_id: uuid::Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_fingerprint: Option<String>,
}

async fn get_api_key_status_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ApiKeyStatusQuery>,
) -> Result<Json<ApiKeyStatusResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&q.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let status = mcp_auth
        .public_key_status(q.config_id)
        .instrument(crate::spans::security_mcp_api_key_status(&q.config_id))
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key status lookup");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let bound = status.is_some();
    let key_fingerprint = status.map(|s| s.key_fingerprint);
    Ok(Json(ApiKeyStatusResponse {
        bound,
        config_id: q.config_id,
        key_fingerprint,
    }))
}

#[derive(Debug, Deserialize)]
struct ListApiKeysQuery {
    config_id: Uuid,
}

async fn list_api_keys_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ListApiKeysQuery>,
) -> Result<Json<Vec<McpApiKeyListItem>>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&q.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    mcp_auth
        .list_api_keys(q.config_id)
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key list");
            map_mcp_key_err(e)
        })
        .map(Json)
}

#[derive(Debug, Deserialize)]
struct RevealApiKeyQuery {
    config_id: Uuid,
    key_id: Uuid,
}

#[derive(Debug, Serialize)]
struct RevealApiKeyResponse {
    api_key: String,
}

async fn reveal_api_key_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<RevealApiKeyQuery>,
) -> Result<Json<RevealApiKeyResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&q.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let api_key = mcp_auth
        .reveal_api_key(q.config_id, q.key_id)
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key reveal");
            map_mcp_key_err(e)
        })?;
    Ok(Json(RevealApiKeyResponse { api_key }))
}

#[derive(Debug, Deserialize)]
struct RevokeOneKeyBody {
    config_id: Uuid,
    key_id: Uuid,
}

async fn revoke_one_api_key_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RevokeOneKeyBody>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&body.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    mcp_auth
        .revoke_one_api_key(body.config_id, body.key_id)
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key revoke one");
            map_mcp_key_err(e)
        })?;
    if let Ok(st) = mcp_auth.public_key_status(body.config_id).await {
        let fp = st.as_ref().map(|s| s.key_fingerprint.as_str());
        let _ = repo.set_mcp_api_key_fingerprint(body.config_id, fp).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct SetKeyLabelBody {
    config_id: Uuid,
    key_id: Uuid,
    label: String,
}

async fn set_key_label_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SetKeyLabelBody>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&body.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let label = require_mcp_key_name(body.label)?;
    mcp_auth
        .set_key_label(body.config_id, body.key_id, label)
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key set label");
            map_mcp_key_err(e)
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct RotateOneKeyBody {
    config_id: Uuid,
    key_id: Uuid,
    /// Name for the replacement key (non-empty after trim; max 128 in registry).
    label: String,
}

async fn rotate_one_api_key_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RotateOneKeyBody>,
) -> Result<Json<ProvisionResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        crate::metrics::record_audit_control_plane("mcp.api_key.rotate_one", "denied");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        crate::metrics::record_audit_control_plane("mcp.api_key.rotate_one", "dependency_error");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&body.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        crate::metrics::record_audit_control_plane("mcp.api_key.rotate_one", "not_found");
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        crate::metrics::record_audit_control_plane("mcp.api_key.rotate_one", "dependency_error");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let label = require_mcp_key_name(body.label)?;
    let p = mcp_auth
        .rotate_one_api_key(body.config_id, body.key_id, label)
        .instrument(crate::spans::security_mcp_api_key_rotate_one(
            &body.config_id,
        ))
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key rotate one");
            map_mcp_key_err(e)
        })?;
    if let Ok(Some(status)) = mcp_auth.public_key_status(body.config_id).await {
        let _ = repo
            .set_mcp_api_key_fingerprint(body.config_id, Some(status.key_fingerprint.as_str()))
            .await;
    }
    crate::metrics::record_audit_control_plane("mcp.api_key.rotate_one", "success");
    Ok(Json(ProvisionResponse {
        api_key: p.api_key,
        key_id: p.key_id,
    }))
}

async fn rotate_api_key_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RotateBody>,
) -> Result<Json<ProvisionResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !repo
        .config_exists(&body.config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let new_name = require_mcp_key_name(body.label)?;
    let p = mcp_auth
        .rotate_api_key(body.config_id, new_name)
        .instrument(crate::spans::security_mcp_api_key_rotate(&body.config_id))
        .await
        .map_err(|e| {
            tracing::warn!(message = %e, "mcp api key rotate");
            map_mcp_key_err(e)
        })?;
    if let Ok(Some(status)) = mcp_auth.public_key_status(body.config_id).await {
        let _ = repo
            .set_mcp_api_key_fingerprint(body.config_id, Some(status.key_fingerprint.as_str()))
            .await;
    }
    Ok(Json(ProvisionResponse {
        api_key: p.api_key,
        key_id: p.key_id,
    }))
}

#[derive(Debug, Deserialize)]
struct RevokeBody {
    id: uuid::Uuid,
}

async fn revoke_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RevokeBody>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "MCP config sync") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let revoke_span = crate::spans::security_mcp_config_revoke(&body.id);
    revoke_span.clone().in_scope(|| {
        tracing::info!(config_id = %body.id, "MCP config revoke");
    });
    repo.revoke_config(body.id).await.map_err(|e| {
        tracing::warn!(message = %e, "mcp config revoke persist");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if let Some(mcp_auth) = st.mcp_transport_auth() {
        let _ = mcp_auth
            .revoke_for_config(body.id)
            .instrument(revoke_span)
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Routes merged under the main HTTP router (before incoming-auth middleware on `/v1/*`).
pub fn mcp_config_routes() -> Router {
    Router::new()
        .route("/internal/mcp-config/v1/upsert", post(upsert_handler))
        .route(
            "/internal/mcp-config/v1/config/{id}",
            get(get_config_detail_handler),
        )
        .route("/internal/mcp-config/v1/list", get(list_configs_handler))
        .route("/internal/mcp-config/v1/revoke", post(revoke_handler))
        .route(
            "/internal/mcp-api-key/v1/provision",
            post(provision_api_key_handler),
        )
        .route(
            "/internal/mcp-api-key/v1/rotate",
            post(rotate_api_key_handler),
        )
        .route(
            "/internal/mcp-api-key/v1/status",
            get(get_api_key_status_handler),
        )
        .route("/internal/mcp-api-key/v1/keys", get(list_api_keys_handler))
        .route(
            "/internal/mcp-api-key/v1/reveal",
            get(reveal_api_key_handler),
        )
        .route(
            "/internal/mcp-api-key/v1/keys/revoke",
            post(revoke_one_api_key_handler),
        )
        .route(
            "/internal/mcp-api-key/v1/keys/label",
            post(set_key_label_handler),
        )
        .route(
            "/internal/mcp-api-key/v1/keys/rotate-one",
            post(rotate_one_api_key_handler),
        )
}
