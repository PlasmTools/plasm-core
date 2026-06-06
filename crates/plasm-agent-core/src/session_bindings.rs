//! Load encrypted MCP binding envelopes for execute sessions.

use std::collections::HashMap;
use std::sync::Arc;

use auth_framework::storage::AuthStorage;

use crate::binding_slots::{BindingScope, SessionBindingMap};
use crate::binding_store::{self, BindingLoadError};
use crate::mcp_config_repository::McpConfigRepository;
use crate::server_state::PlasmHostState;

pub use crate::binding_store::BindingLoadError as SessionBindingLoadError;

pub async fn load_session_binding_map(
    storage: &Arc<dyn AuthStorage>,
    repo: &McpConfigRepository,
    scope: &BindingScope,
) -> Result<SessionBindingMap, String> {
    binding_store::load_session_binding_map(storage, repo, scope)
        .await
        .map_err(|e| e.to_string())
}

pub async fn tenant_bindings_for_entries(
    st: &PlasmHostState,
    cfg: &crate::mcp_runtime_config::McpRuntimeConfig,
    entry_ids: &[String],
) -> Result<HashMap<String, SessionBindingMap>, String> {
    let Some(repo) = st.mcp_config_repository() else {
        return Ok(HashMap::new());
    };
    let Some(storage) = st.auth_storage() else {
        return Ok(HashMap::new());
    };
    let storage = storage.clone();
    let mut futs = Vec::new();
    for eid in entry_ids {
        if !cfg.auth_config_by_entry.contains_key(eid) {
            continue;
        }
        if !crate::binding_slots::entry_requires_bindings(eid) {
            continue;
        }
        let scope = BindingScope::new(cfg.tenant_id.clone(), cfg.id, eid.clone());
        let storage = Arc::clone(&storage);
        let eid = eid.clone();
        futs.push(async move {
            let values = binding_store::load_binding_values_scoped(&storage, repo, &scope).await?;
            match values {
                Some(vals) if crate::binding_slots::bindings_complete_for_entry(&eid, &vals) => {
                    Ok(Some((eid, SessionBindingMap::from_values(scope, vals))))
                }
                Some(_) => Err(BindingLoadError::Incomplete(eid)),
                None => Err(BindingLoadError::NotConfigured(eid)),
            }
        });
    }
    let results = futures_util::future::join_all(futs).await;
    let mut out = HashMap::new();
    for result in results {
        match result {
            Ok(Some((eid, map))) => {
                out.insert(eid, map);
            }
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}

/// Synthetic binding map for local `--backend` / REPL (no MCP scope).
pub fn repl_session_binding_map(entry_id: &str, http_backend: &str) -> Option<SessionBindingMap> {
    if !crate::binding_slots::catalog_http_backend_needs_origin(entry_id, http_backend) {
        return None;
    }
    let trimmed = http_backend.trim().trim_end_matches('/');
    if trimmed.is_empty()
        || crate::catalog_ownership::is_fibery_account_placeholder_http_backend(trimmed)
    {
        return None;
    }
    let mut values = indexmap::IndexMap::new();
    values.insert(
        crate::binding_slots::BindingSlot::CatalogHttpOrigin
            .wire_name()
            .to_string(),
        trimmed.to_string(),
    );
    Some(SessionBindingMap {
        scope: None,
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repl_binding_from_backend_flag() {
        let map = repl_session_binding_map("fibery", "https://acme.fibery.io").expect("map");
        assert_eq!(
            map.get_wire("catalog_http_origin"),
            Some("https://acme.fibery.io")
        );
        assert_eq!(
            map.cml_env_entries()
                .get("bind_catalog_http_origin")
                .map(String::as_str),
            Some("https://acme.fibery.io")
        );
    }
}
