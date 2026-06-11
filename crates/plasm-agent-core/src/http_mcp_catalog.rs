//! Tenant-scoped catalog listing for MCP App bootstrap UIs.

use axum::extract::Extension;
use axum::http::HeaderMap;
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use plasm_core::discovery::CgsCatalog;
use serde::Serialize;

use crate::mcp_policy;
use crate::server_state::PlasmHostState;

#[derive(Debug, Serialize)]
pub struct EnabledCatalogsResponse {
    /// `None` when no tenant MCP policy applies (all registry entries allowed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_ids: Option<Vec<String>>,
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

async fn get_enabled_catalogs(
    Extension(st): Extension<PlasmHostState>,
    headers: HeaderMap,
) -> Json<EnabledCatalogsResponse> {
    let reg = st.catalog.snapshot();
    let Some(repo) = st.mcp_config_repository() else {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    };
    if !repo.has_tenant_configs().await.unwrap_or(false) {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    }
    let Some(token) = bearer_token(&headers) else {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    };
    let Some(mcp_auth) = st.mcp_transport_auth() else {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    };
    let Some(config_id) = mcp_auth.verify_api_key(token).await else {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    };
    let Some(cfg) = repo
        .get_runtime_config(&config_id)
        .await
        .ok()
        .flatten()
    else {
        return Json(EnabledCatalogsResponse { entry_ids: None });
    };
    let mut entry_ids: Vec<String> = mcp_policy::filter_registry_entries(reg.list_entries(), &cfg)
        .into_iter()
        .map(|m| m.entry_id)
        .collect();
    entry_ids.sort();
    Json(EnabledCatalogsResponse {
        entry_ids: Some(entry_ids),
    })
}

pub fn mcp_catalog_routes() -> Router {
    Router::new().route("/v1/mcp/enabled-catalogs", get(get_enabled_catalogs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parses_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer test-key".parse().unwrap(),
        );
        assert_eq!(bearer_token(&headers), Some("test-key"));
    }
}
