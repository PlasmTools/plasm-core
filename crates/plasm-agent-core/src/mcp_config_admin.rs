//! Shared MCP configuration administration over [`crate::mcp_config_repository::McpConfigRepository`]
//! and [`crate::mcp_transport_auth::McpTransportAuth`] — **no HTTP loopback**.
//!
//! Callers (hosted Phoenix adapters, `plasm-server`, tests) supply workspace scope identity.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use auth_framework::errors::AuthError;
use plasm_core::discovery::{CgsCatalog, InMemoryCgsRegistry};
use plasm_core::{
    catalog_connect_profile, AuthScheme, CatalogAuthCapability, CatalogConnectProfile,
    CatalogOauthCapability,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::mcp_api_key_registry::{McpApiKeyListItem, McpApiKeyProvisioned};
use crate::mcp_config_repository::{McpConfigRepository, McpConfigRepositoryError};
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::mcp_transport_auth::McpTransportAuth;

/// Workspace / tenant identity for one MCP policy row (`project_mcp_configs`).
#[derive(Debug, Clone)]
pub struct McpConfigScope {
    pub tenant_id: String,
    pub workspace_slug: String,
    pub project_slug: String,
    pub space_type: String,
    pub owner_subject: Option<String>,
}

impl McpConfigScope {
    pub fn organization_workspace_project(
        tenant_id: String,
        workspace_slug: String,
        project_slug: String,
    ) -> Self {
        Self {
            tenant_id,
            workspace_slug,
            project_slug,
            space_type: "organization".into(),
            owner_subject: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum McpConfigAdminError {
    #[error("MCP policy store (project_mcp_*) is not available")]
    PolicyStoreUnavailable,
    #[error("configuration not found: {0}")]
    ConfigNotFound(Uuid),
    #[error("invalid UUID: {0}")]
    InvalidUuid(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Repo(#[from] McpConfigRepositoryError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("{0}")]
    Msg(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfigAdminSummary {
    pub config_id: Uuid,
    pub name: String,
    pub status: String,
    pub tenant_id: String,
    pub workspace_slug: String,
    pub project_slug: String,
    pub space_type: String,
    pub owner_subject: Option<String>,
    pub config_version: u64,
    pub enabled_api_count: usize,
    pub api_key_count: usize,
    pub selected_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpCatalogAuthMarker {
    Public,
    RequiresConnect,
    AuthOptionalListed,
    MissingBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfigCatalogRow {
    pub entry_id: String,
    pub label: String,
    pub enabled_for_mcp: bool,
    pub auth_optional: bool,
    pub has_auth_binding: bool,
    pub auth_marker: McpCatalogAuthMarker,
    pub connect_profile: CatalogConnectProfile,
    #[serde(default)]
    pub auth_scheme_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_secret_hosted_kv: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub api_secret_present: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bindings_required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bindings_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfigApiKeyRow {
    pub key_id: Uuid,
    pub fingerprint: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpConfigAuthCoverage {
    pub optional_entry_ids: Vec<String>,
    pub entries_missing_binding_when_required: Vec<String>,
}

/// Administrative façade used by UIs and non-interactive CLIs.
#[derive(Clone)]
pub struct McpConfigAdminService {
    repo: Arc<McpConfigRepository>,
    keys: Arc<dyn McpTransportAuth>,
}

impl McpConfigAdminService {
    pub fn new(repo: Arc<McpConfigRepository>, keys: Arc<dyn McpTransportAuth>) -> Self {
        Self { repo, keys }
    }

    pub fn repo(&self) -> &Arc<McpConfigRepository> {
        &self.repo
    }

    pub fn keys(&self) -> &Arc<dyn McpTransportAuth> {
        &self.keys
    }

    /// Prefer `preferred_id` when it matches the scope row; otherwise newest row for the triple;
    /// **`None`** when nothing exists (does not create).
    pub async fn resolve_existing_config_id(
        &self,
        scope: &McpConfigScope,
        preferred_id: Option<Uuid>,
    ) -> Result<Option<Uuid>, McpConfigAdminError> {
        if let Some(pid) = preferred_id {
            if config_matches_scope(self.repo.as_ref(), pid, scope).await? {
                return Ok(Some(pid));
            }
        }
        let list = self
            .repo
            .list_configs_by_scope_json(
                scope.tenant_id.trim(),
                scope.workspace_slug.trim(),
                scope.project_slug.trim(),
                None,
                None,
            )
            .await?;
        Ok(first_config_id_from_list_json(&list))
    }

    /// Ensure exactly one active config exists for the `(tenant, workspace, project)` triple,
    /// creating it when missing. New rows use `scope.space_type` / `owner_subject`.
    pub async fn ensure_singleton_config(
        &self,
        scope: &McpConfigScope,
        preferred_id: Option<Uuid>,
        name: &str,
    ) -> Result<Uuid, McpConfigAdminError> {
        if let Some(existing) = self.resolve_existing_config_id(scope, preferred_id).await? {
            return Ok(existing);
        }
        let id = preferred_id.unwrap_or_else(Uuid::new_v4);
        let endpoint_secret_hash = random_endpoint_secret_hash();
        let runtime = McpRuntimeConfig {
            id,
            tenant_id: scope.tenant_id.trim().to_string(),
            space_type: normalize_space_type(&scope.space_type),
            owner_subject: scope
                .owner_subject
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            version: 1,
            endpoint_secret_hash,
            credential_secret_hashes: HashSet::new(),
            allowed_entry_ids: HashSet::new(),
            capabilities_by_entry: HashMap::new(),
            auth_config_by_entry: HashMap::new(),
        };
        let nm = name.trim();
        let nm = if nm.is_empty() { "Your MCP" } else { nm };
        self.repo
            .upsert_full(
                runtime,
                scope.workspace_slug.trim(),
                scope.project_slug.trim(),
                nm,
                "active",
                &[] as &[String],
            )
            .await?;
        Ok(id)
    }

    pub async fn admin_summary(
        &self,
        config_id: Uuid,
    ) -> Result<McpConfigAdminSummary, McpConfigAdminError> {
        let Some(detail) = self.repo.get_config_detail_json(&config_id).await? else {
            return Err(McpConfigAdminError::ConfigNotFound(config_id));
        };
        let keys = self.keys.list_api_keys(config_id).await?;
        summary_from_detail_json(&detail, keys.len())
    }

    pub async fn list_api_key_rows(
        &self,
        config_id: Uuid,
    ) -> Result<Vec<McpConfigApiKeyRow>, McpConfigAdminError> {
        let keys = self.keys.list_api_keys(config_id).await?;
        Ok(keys.into_iter().map(into_api_key_row).collect())
    }

    pub async fn provision_api_key(
        &self,
        config_id: Uuid,
        label: String,
    ) -> Result<McpApiKeyProvisioned, McpConfigAdminError> {
        Ok(self.keys.provision_api_key(config_id, label).await?)
    }

    pub async fn reveal_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<String, McpConfigAdminError> {
        Ok(self.keys.reveal_api_key(config_id, key_id).await?)
    }

    pub async fn rotate_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        new_label: String,
    ) -> Result<McpApiKeyProvisioned, McpConfigAdminError> {
        Ok(self
            .keys
            .rotate_one_api_key(config_id, key_id, new_label)
            .await?)
    }

    pub async fn revoke_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<(), McpConfigAdminError> {
        Ok(self.keys.revoke_one_api_key(config_id, key_id).await?)
    }

    /// Merge registry [`InMemoryCgsRegistry`] metadata with DB-backed allowgraph + optional-auth flags.
    pub fn catalog_rows(
        registry: &InMemoryCgsRegistry,
        runtime: &McpRuntimeConfig,
        auth_optional: &HashSet<String>,
    ) -> Vec<McpConfigCatalogRow> {
        let mut ids: BTreeSet<String> = registry
            .list_entries()
            .into_iter()
            .map(|m| m.entry_id)
            .collect();
        for e in &runtime.allowed_entry_ids {
            ids.insert(e.clone());
        }
        let mut rows = Vec::with_capacity(ids.len());
        for entry_id in ids {
            let profile = connect_profile_for_entry(registry, &entry_id);
            let (auth_scheme_summary, api_secret_hosted_kv) =
                auth_scheme_summary_for_entry(registry, &entry_id);
            let marker = auth_marker_for_row(
                &profile,
                auth_optional.contains(&entry_id),
                runtime,
                &entry_id,
            );
            let label = registry
                .lookup_entry_meta(&entry_id)
                .map(|m| m.label)
                .unwrap_or_else(|| entry_id.clone());
            rows.push(McpConfigCatalogRow {
                entry_id: entry_id.clone(),
                label,
                enabled_for_mcp: runtime.allowed_entry_ids.contains(&entry_id),
                auth_optional: auth_optional.contains(&entry_id),
                has_auth_binding: runtime.auth_config_by_entry.contains_key(&entry_id),
                auth_marker: marker,
                connect_profile: profile,
                auth_scheme_summary,
                api_secret_hosted_kv,
                api_secret_present: false,
                bindings_required: crate::binding_slots::entry_requires_bindings(&entry_id),
                bindings_complete: false,
            });
        }
        rows
    }

    pub fn auth_coverage(
        rows: &[McpConfigCatalogRow],
        optional_ids: &[String],
    ) -> McpConfigAuthCoverage {
        let optional_set: HashSet<String> = optional_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut missing = Vec::new();
        for r in rows {
            if !r.enabled_for_mcp {
                continue;
            }
            if matches!(
                r.auth_marker,
                McpCatalogAuthMarker::RequiresConnect | McpCatalogAuthMarker::MissingBinding
            ) && !r.has_auth_binding
            {
                missing.push(r.entry_id.clone());
            }
        }
        let mut optional_entry_ids: Vec<String> = optional_set.into_iter().collect();
        optional_entry_ids.sort();
        missing.sort();
        missing.dedup();
        McpConfigAuthCoverage {
            optional_entry_ids,
            entries_missing_binding_when_required: missing,
        }
    }

    /// Replace the enabled API set (capabilities remain “all” via empty per-entry sets).
    pub async fn set_allowed_apis_exact(
        &self,
        config_id: Uuid,
        allowed: HashSet<String>,
    ) -> Result<(), McpConfigAdminError> {
        let Some(mut runtime) = self.repo.get_runtime_config(&config_id).await? else {
            return Err(McpConfigAdminError::ConfigNotFound(config_id));
        };
        let meta = fetch_config_row_meta(self.repo.as_ref(), config_id)
            .await?
            .ok_or(McpConfigAdminError::ConfigNotFound(config_id))?;
        sync_capabilities_with_allowed(&mut runtime, &allowed);
        runtime.allowed_entry_ids = allowed;
        runtime.version = runtime.version.saturating_add(1).max(1);
        self.repo
            .upsert_full(
                runtime,
                &meta.workspace_slug,
                &meta.project_slug,
                &meta.name,
                &meta.status,
                &meta.auth_optional_entry_ids,
            )
            .await?;
        Ok(())
    }

    pub async fn enable_api(
        &self,
        config_id: Uuid,
        entry_id: &str,
    ) -> Result<(), McpConfigAdminError> {
        self.toggle_api(config_id, entry_id, true).await
    }

    pub async fn disable_api(
        &self,
        config_id: Uuid,
        entry_id: &str,
    ) -> Result<(), McpConfigAdminError> {
        self.toggle_api(config_id, entry_id, false).await
    }

    async fn toggle_api(
        &self,
        config_id: Uuid,
        entry_id: &str,
        on: bool,
    ) -> Result<(), McpConfigAdminError> {
        let e = entry_id.trim();
        if e.is_empty() {
            return Err(McpConfigAdminError::Msg(
                "entry_id must be non-empty".into(),
            ));
        }
        let Some(mut runtime) = self.repo.get_runtime_config(&config_id).await? else {
            return Err(McpConfigAdminError::ConfigNotFound(config_id));
        };
        let meta = fetch_config_row_meta(self.repo.as_ref(), config_id)
            .await?
            .ok_or(McpConfigAdminError::ConfigNotFound(config_id))?;
        if on {
            runtime.allowed_entry_ids.insert(e.to_string());
            runtime
                .capabilities_by_entry
                .entry(e.to_string())
                .or_default();
        } else {
            runtime.allowed_entry_ids.remove(e);
            runtime.capabilities_by_entry.remove(e);
            runtime.auth_config_by_entry.remove(e);
        }
        runtime.version = runtime.version.saturating_add(1).max(1);
        self.repo
            .upsert_full(
                runtime,
                &meta.workspace_slug,
                &meta.project_slug,
                &meta.name,
                &meta.status,
                &meta.auth_optional_entry_ids,
            )
            .await?;
        Ok(())
    }

    pub async fn load_runtime_snapshot(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpRuntimeConfig>, McpConfigAdminError> {
        Ok(self.repo.get_runtime_config(&config_id).await?)
    }

    pub async fn load_auth_optional_set(
        &self,
        config_id: Uuid,
    ) -> Result<HashSet<String>, McpConfigAdminError> {
        let meta = fetch_config_row_meta(self.repo.as_ref(), config_id)
            .await?
            .ok_or(McpConfigAdminError::ConfigNotFound(config_id))?;
        Ok(meta.auth_optional_entry_ids.into_iter().collect())
    }
}

#[derive(Debug)]
struct ConfigRowMeta {
    workspace_slug: String,
    project_slug: String,
    name: String,
    status: String,
    auth_optional_entry_ids: Vec<String>,
}

async fn fetch_config_row_meta(
    repo: &McpConfigRepository,
    config_id: Uuid,
) -> Result<Option<ConfigRowMeta>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT workspace_slug, project_slug, name, status, auth_optional_entry_ids
           FROM project_mcp_configs WHERE id = $1"#,
    )
    .bind(config_id)
    .fetch_optional(repo.pool())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ConfigRowMeta {
        workspace_slug: row.get::<String, _>("workspace_slug"),
        project_slug: row.get::<String, _>("project_slug"),
        name: row.get::<String, _>("name"),
        status: row.get::<String, _>("status"),
        auth_optional_entry_ids: row.get::<Vec<String>, _>("auth_optional_entry_ids"),
    }))
}

async fn config_matches_scope(
    repo: &McpConfigRepository,
    id: Uuid,
    scope: &McpConfigScope,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM project_mcp_configs
           WHERE id = $1 AND tenant_id = $2 AND workspace_slug = $3 AND project_slug = $4"#,
    )
    .bind(id)
    .bind(scope.tenant_id.trim())
    .bind(scope.workspace_slug.trim())
    .bind(scope.project_slug.trim())
    .fetch_one(repo.pool())
    .await?;
    Ok(n > 0)
}

fn first_config_id_from_list_json(v: &Value) -> Option<Uuid> {
    let cfgs = v.get("configs")?.as_array()?;
    let first = cfgs.first()?;
    let id_s = first.get("id")?.as_str()?;
    Uuid::parse_str(id_s).ok()
}

fn random_endpoint_secret_hash() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    let out = hasher.finalize();
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

fn normalize_space_type(raw: &str) -> String {
    match raw.trim() {
        "personal" => "personal".into(),
        _ => "organization".into(),
    }
}

fn into_api_key_row(k: McpApiKeyListItem) -> McpConfigApiKeyRow {
    McpConfigApiKeyRow {
        key_id: k.key_id,
        fingerprint: k.key_fingerprint,
        label: k.label,
    }
}

fn summary_from_detail_json(
    detail: &Value,
    api_key_count: usize,
) -> Result<McpConfigAdminSummary, McpConfigAdminError> {
    let config_id = Uuid::parse_str(
        detail
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpConfigAdminError::Msg("detail.id missing".into()))?,
    )
    .map_err(|_| McpConfigAdminError::Msg("detail.id not uuid".into()))?;
    let allowed = detail
        .get("allowed_graphs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let enabled_api_count = allowed
        .iter()
        .filter(|g| g.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false))
        .count();
    Ok(McpConfigAdminSummary {
        config_id,
        name: detail
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        status: detail
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        tenant_id: detail
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        workspace_slug: detail
            .get("workspace_slug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        project_slug: detail
            .get("project_slug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        space_type: detail
            .get("space_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        owner_subject: detail
            .get("owner_subject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        config_version: detail
            .get("config_version")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64,
        enabled_api_count,
        api_key_count,
        selected_key_fingerprint: detail
            .get("mcp_api_key_fingerprint")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn connect_profile_for_entry(
    registry: &InMemoryCgsRegistry,
    entry_id: &str,
) -> CatalogConnectProfile {
    let Ok(ctx) = registry.load_context(entry_id) else {
        return CatalogConnectProfile {
            capability: CatalogAuthCapability::Public,
            oauth: CatalogOauthCapability {
                provider_present: false,
                scope_catalog_present: false,
            },
            has_public_mode: true,
            has_api_key: false,
            has_oauth: false,
        };
    };
    catalog_connect_profile(ctx.auth.as_ref(), ctx.oauth.as_ref())
}

fn auth_scheme_summary_for_entry(
    registry: &InMemoryCgsRegistry,
    entry_id: &str,
) -> (String, Option<String>) {
    let Ok(ctx) = registry.load_context(entry_id) else {
        return ("public".into(), None);
    };
    match ctx.cgs.auth.as_ref() {
        None | Some(AuthScheme::None) => ("public".into(), None),
        Some(AuthScheme::ApiKeyHeader {
            header, hosted_kv, ..
        }) => (format!("api key header {header}"), hosted_kv.clone()),
        Some(AuthScheme::ApiKeyQuery {
            param, hosted_kv, ..
        }) => (format!("api key query {param}"), hosted_kv.clone()),
        Some(AuthScheme::BearerToken { hosted_kv, .. }) => {
            ("bearer token".into(), hosted_kv.clone())
        }
        Some(AuthScheme::OauthBearer { hosted_kv, .. }) => {
            ("oauth bearer".into(), hosted_kv.clone())
        }
        Some(AuthScheme::Oauth2ClientCredentials { .. }) => {
            ("oauth2 client credentials".into(), None)
        }
    }
}

fn auth_marker_for_row(
    profile: &CatalogConnectProfile,
    listed_optional: bool,
    runtime: &McpRuntimeConfig,
    entry_id: &str,
) -> McpCatalogAuthMarker {
    if listed_optional {
        return McpCatalogAuthMarker::AuthOptionalListed;
    }
    let requires_connect = profile.capability != CatalogAuthCapability::Public;
    if requires_connect && !runtime.auth_config_by_entry.contains_key(entry_id) {
        return McpCatalogAuthMarker::MissingBinding;
    }
    if requires_connect {
        McpCatalogAuthMarker::RequiresConnect
    } else {
        McpCatalogAuthMarker::Public
    }
}

fn sync_capabilities_with_allowed(runtime: &mut McpRuntimeConfig, allowed: &HashSet<String>) {
    runtime
        .capabilities_by_entry
        .retain(|k, _| allowed.contains(k));
    for e in allowed {
        runtime.capabilities_by_entry.entry(e.clone()).or_default();
    }
}
