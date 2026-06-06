//! Control-plane: store MCP catalog binding envelopes in auth-framework `kv_store`.

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

use crate::binding_slots::{normalize_connect_binding_values, BindingScope};
use crate::binding_store::{self, BindingStoreError};
use crate::control_plane_http::internal_or_outbound_setup_authorized;
use crate::server_state::PlasmHostState;
use plasm_runtime::binding_kv::BINDING_KV_PREFIX;

#[derive(Debug, Deserialize)]
struct PutScopedBody {
    scope: ScopeBody,
    values: HashMap<String, String>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScopeBody {
    tenant_id: String,
    mcp_config_id: String,
    entry_id: String,
}

#[derive(Debug, Deserialize)]
struct DeleteScopedBody {
    tenant_id: String,
    mcp_config_id: String,
    entry_id: String,
}

fn validate_binding_key(key: &str) -> bool {
    key.starts_with(BINDING_KV_PREFIX) && key.len() <= 512
}

async fn put_scoped_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PutScopedBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "binding write") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(storage) = st.auth_storage() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let config_id = Uuid::parse_str(body.scope.mcp_config_id.trim()).map_err(|_| {
        tracing::warn!("binding put: invalid mcp_config_id");
        StatusCode::BAD_REQUEST
    })?;
    let tenant_id = body.scope.tenant_id.trim();
    let entry_id = body.scope.entry_id.trim();
    if tenant_id.is_empty() || entry_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let cfg = repo
        .load_runtime_for_config_including_disabled(config_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "binding put: config load failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    if cfg.tenant_id != tenant_id {
        tracing::warn!(
            expected = %cfg.tenant_id,
            got = %tenant_id,
            "binding put: tenant mismatch"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    if !cfg.allowed_entry_ids.contains(entry_id) {
        tracing::warn!(entry_id = %entry_id, "binding put: entry not on MCP config");
        return Err(StatusCode::BAD_REQUEST);
    }
    let normalized = normalize_connect_binding_values(entry_id, &body.values).map_err(|e| {
        tracing::warn!(error = %e, "binding put: invalid values");
        StatusCode::BAD_REQUEST
    })?;
    let key_override = body
        .key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(key) = key_override {
        if !validate_binding_key(key) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let scope = BindingScope::new(tenant_id, config_id, entry_id);
    let key = binding_store::store_scoped_binding_envelope(
        storage,
        repo,
        scope,
        normalized,
        key_override,
    )
    .await
    .map_err(|e| map_binding_store_err(e, config_id, entry_id))?;
    tracing::info!(
        tenant_id = %tenant_id,
        config_id = %config_id,
        entry_id = %entry_id,
        key = %key,
        "mcp binding envelope stored"
    );
    Ok(Json(serde_json::json!({ "key": key })))
}

async fn delete_scoped_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DeleteScopedBody>,
) -> Result<StatusCode, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "binding write") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(storage) = st.auth_storage() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(repo) = st.mcp_config_repository() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let config_id = Uuid::parse_str(body.mcp_config_id.trim()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let tenant_id = body.tenant_id.trim();
    let entry_id = body.entry_id.trim();
    if tenant_id.is_empty() || entry_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let cfg = repo
        .load_runtime_for_config_including_disabled(config_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if cfg.tenant_id != tenant_id {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Ok(Some(key)) = repo.fetch_binding_kv_key_for_scope(config_id, entry_id).await {
        let _ = storage.delete_kv(key.trim()).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn map_binding_store_err(e: BindingStoreError, config_id: Uuid, entry_id: &str) -> StatusCode {
    tracing::warn!(
        config_id = %config_id,
        entry_id = %entry_id,
        error = %e,
        "binding store failed"
    );
    StatusCode::INTERNAL_SERVER_ERROR
}

pub fn mcp_bindings_routes() -> Router {
    Router::new()
        .route(
            "/internal/mcp-bindings/v1/put-scoped",
            post(put_scoped_handler),
        )
        .route(
            "/internal/mcp-bindings/v1/delete-scoped",
            post(delete_scoped_handler),
        )
}
