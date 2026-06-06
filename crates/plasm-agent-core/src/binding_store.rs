//! Scoped MCP binding KV store/load — single primitive for HTTP, TUI, readiness, and execute.

use auth_framework::storage::AuthStorage;
use indexmap::IndexMap;
use plasm_runtime::binding_kv::{
    binding_kv_key_from_uuid, parse_binding_kv_v1_scoped, BindingKvV1, BindingScopeV1,
    BINDING_KV_VERSION,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::binding_slots::{
    bindings_complete_for_entry, entry_requires_bindings, BindingScope, SessionBindingMap,
};
use crate::mcp_config_repository::McpConfigRepository;

#[derive(Debug, thiserror::Error)]
pub enum BindingLoadError {
    #[error("binding pointer lookup failed: {0}")]
    PointerLookup(#[from] sqlx::Error),
    #[error("binding KV read failed: {0}")]
    KvRead(String),
    #[error("binding KV missing for key {0}")]
    KvMissing(String),
    #[error("binding envelope invalid: {0}")]
    Parse(String),
    #[error("binding not configured for catalog `{0}`")]
    NotConfigured(String),
    #[error("binding incomplete for catalog `{0}`")]
    Incomplete(String),
}

#[derive(Debug, thiserror::Error)]
pub enum BindingStoreError {
    #[error("binding store failed: {0}")]
    KvStore(String),
    #[error("binding pointer upsert failed: {0}")]
    PointerUpsert(#[from] sqlx::Error),
    #[error("serialization failed")]
    Serialize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessGapKind {
    Secret,
    Binding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessGap {
    pub entry_id: String,
    pub gap: ReadinessGapKind,
}

impl From<&BindingScope> for BindingScopeV1 {
    fn from(scope: &BindingScope) -> Self {
        BindingScopeV1 {
            tenant_id: scope.tenant_id.clone(),
            mcp_config_id: scope.mcp_config_id.to_string(),
            entry_id: scope.entry_id.clone(),
        }
    }
}

/// Load binding wire values for a scope. `Ok(None)` when no Postgres pointer exists.
pub async fn load_binding_values_scoped(
    storage: &Arc<dyn AuthStorage>,
    repo: &McpConfigRepository,
    scope: &BindingScope,
) -> Result<Option<IndexMap<String, String>>, BindingLoadError> {
    let mcp_config_id = scope.mcp_config_id.to_string();
    let Some(key) = repo
        .fetch_binding_kv_key_for_scope(scope.mcp_config_id, &scope.entry_id)
        .await?
    else {
        return Ok(None);
    };
    let key_trim = key.trim();
    let Some(bytes) = storage
        .get_kv(key_trim)
        .await
        .map_err(|e| BindingLoadError::KvRead(e.to_string()))?
    else {
        return Err(BindingLoadError::KvMissing(key));
    };
    let raw = String::from_utf8_lossy(&bytes);
    let env = parse_binding_kv_v1_scoped(
        raw.trim(),
        &scope.tenant_id,
        &mcp_config_id,
        &scope.entry_id,
    )
    .map_err(|e| BindingLoadError::Parse(e.to_string()))?;
    Ok(Some(env.values.into_iter().collect()))
}

pub async fn load_session_binding_map(
    storage: &Arc<dyn AuthStorage>,
    repo: &McpConfigRepository,
    scope: &BindingScope,
) -> Result<SessionBindingMap, BindingLoadError> {
    match load_binding_values_scoped(storage, repo, scope).await? {
        Some(values) => Ok(SessionBindingMap::from_values(scope.clone(), values)),
        None => Ok(SessionBindingMap::empty()),
    }
}

/// Store a scoped binding envelope, upsert the Postgres pointer, and delete the prior KV key when rotated.
pub async fn store_scoped_binding_envelope(
    storage: &Arc<dyn AuthStorage>,
    repo: &McpConfigRepository,
    scope: BindingScope,
    values: IndexMap<String, String>,
    key_override: Option<&str>,
) -> Result<String, BindingStoreError> {
    let old_key = repo
        .fetch_binding_kv_key_for_scope(scope.mcp_config_id, &scope.entry_id)
        .await
        .ok()
        .flatten();
    let key = key_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| binding_kv_key_from_uuid(&Uuid::new_v4().to_string()));
    let envelope = BindingKvV1 {
        version: BINDING_KV_VERSION,
        scope: BindingScopeV1::from(&scope),
        values: values.into_iter().collect::<HashMap<_, _>>(),
    };
    let json = serde_json::to_string(&envelope).map_err(|_| BindingStoreError::Serialize)?;
    storage
        .store_kv(key.as_str(), json.as_bytes(), None)
        .await
        .map_err(|e| BindingStoreError::KvStore(e.to_string()))?;
    repo.upsert_entry_binding(scope, key.as_str()).await?;
    if let Some(old) = old_key {
        let old_trim = old.trim();
        if !old_trim.is_empty() && old_trim != key.as_str() {
            let _ = storage.delete_kv(old_trim).await;
        }
    }
    Ok(key)
}

pub fn bindings_complete_for_values(entry_id: &str, values: &IndexMap<String, String>) -> bool {
    bindings_complete_for_entry(entry_id, values)
}

pub async fn entry_bindings_complete_scoped(
    storage: &Arc<dyn AuthStorage>,
    repo: &McpConfigRepository,
    scope: &BindingScope,
) -> bool {
    if !entry_requires_bindings(&scope.entry_id) {
        return true;
    }
    match load_binding_values_scoped(storage, repo, scope).await {
        Ok(Some(values)) => bindings_complete_for_values(&scope.entry_id, &values),
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                target: "plasm_agent::binding_store",
                config_id = %scope.mcp_config_id,
                entry_id = %scope.entry_id,
                error = %e,
                "binding completeness check failed"
            );
            false
        }
    }
}

pub async fn entry_secret_present(
    repo: &McpConfigRepository,
    storage: Option<&Arc<dyn AuthStorage>>,
    config_id: Uuid,
    entry_id: &str,
) -> bool {
    let Some(storage) = storage else {
        return false;
    };
    let Ok(Some(kv_key)) = repo
        .fetch_hosted_kv_for_graph_binding(config_id, entry_id, None)
        .await
    else {
        return false;
    };
    storage
        .get_kv(kv_key.trim())
        .await
        .ok()
        .flatten()
        .is_some_and(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_v1_from_binding_scope() {
        let scope = BindingScope::new("t1", Uuid::nil(), "fibery");
        let v1 = BindingScopeV1::from(&scope);
        assert_eq!(v1.tenant_id, "t1");
        assert_eq!(v1.entry_id, "fibery");
    }
}
