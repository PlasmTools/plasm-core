//! MCP bundle activation readiness (secret + binding slots).

use std::collections::HashSet;
use std::sync::Arc;

use auth_framework::storage::AuthStorage;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::schema::AuthScheme;
use plasm_core::CgsCatalog;

use crate::binding_slots::BindingScope;
use crate::binding_store::{self, ReadinessGap, ReadinessGapKind};
use crate::mcp_config_repository::McpConfigRepository;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::server_state::PlasmHostState;

pub async fn catalog_entry_readiness_gaps(
    repo: &McpConfigRepository,
    storage: Option<&Arc<dyn AuthStorage>>,
    cfg: &McpRuntimeConfig,
    entry_id: &str,
    optional: &HashSet<String>,
    requires_auth: bool,
) -> Vec<ReadinessGap> {
    if optional.contains(entry_id) || !requires_auth {
        return Vec::new();
    }
    if !cfg.auth_config_by_entry.contains_key(entry_id) {
        return vec![ReadinessGap {
            entry_id: entry_id.to_string(),
            gap: ReadinessGapKind::Secret,
        }];
    }
    let scope = BindingScope::new(cfg.tenant_id.clone(), cfg.id, entry_id.to_string());
    let (secret_ok, binding_ok) = if let Some(storage) = storage {
        tokio::join!(
            binding_store::entry_secret_present(repo, Some(storage), cfg.id, entry_id),
            binding_store::entry_bindings_complete_scoped(storage, repo, &scope),
        )
    } else {
        (false, false)
    };
    let mut gaps = Vec::new();
    if !secret_ok {
        gaps.push(ReadinessGap {
            entry_id: entry_id.to_string(),
            gap: ReadinessGapKind::Secret,
        });
    }
    if !binding_ok {
        gaps.push(ReadinessGap {
            entry_id: entry_id.to_string(),
            gap: ReadinessGapKind::Binding,
        });
    }
    gaps
}

pub async fn catalog_entry_ready(
    st: &PlasmHostState,
    cfg: &McpRuntimeConfig,
    entry_id: &str,
    optional: &HashSet<String>,
    requires_auth: bool,
) -> bool {
    let Some(repo) = st.mcp_config_repository() else {
        return false;
    };
    catalog_entry_readiness_gaps(
        repo,
        st.auth_storage(),
        cfg,
        entry_id,
        optional,
        requires_auth,
    )
    .await
    .is_empty()
}

/// Returns sorted readiness gaps blocking an **active** MCP config sync.
pub async fn active_config_readiness_gaps(
    st: &PlasmHostState,
    cfg: &McpRuntimeConfig,
    optional_entry_ids: &[String],
    registry: &InMemoryCgsRegistry,
) -> Vec<ReadinessGap> {
    let Some(repo) = st.mcp_config_repository() else {
        return cfg
            .allowed_entry_ids
            .iter()
            .map(|entry_id| ReadinessGap {
                entry_id: entry_id.clone(),
                gap: ReadinessGapKind::Secret,
            })
            .collect();
    };
    let storage = st.auth_storage();
    let optional: HashSet<String> = optional_entry_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut gaps = Vec::new();
    for entry_id in &cfg.allowed_entry_ids {
        let requires_auth = registry
            .load_context(entry_id)
            .ok()
            .map(|ctx| {
                ctx.cgs
                    .auth
                    .as_ref()
                    .map(|a| !matches!(a, AuthScheme::None))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        gaps.extend(
            catalog_entry_readiness_gaps(
                repo,
                storage,
                cfg,
                entry_id,
                &optional,
                requires_auth,
            )
            .await,
        );
    }
    gaps.sort_by(|a, b| a.entry_id.cmp(&b.entry_id).then_with(|| format!("{:?}", a.gap).cmp(&format!("{:?}", b.gap))));
    gaps.dedup_by(|a, b| a.entry_id == b.entry_id && a.gap == b.gap);
    gaps
}

pub async fn hydrate_catalog_row_connect_status(
    st: &PlasmHostState,
    cfg: &McpRuntimeConfig,
    row: &mut crate::mcp_config_admin::McpConfigCatalogRow,
) {
    let Some(repo) = st.mcp_config_repository() else {
        return;
    };
    let storage = st.auth_storage();
    let scope = BindingScope::new(cfg.tenant_id.clone(), cfg.id, row.entry_id.clone());
    let (secret_ok, binding_ok) = if let Some(storage) = storage {
        tokio::join!(
            binding_store::entry_secret_present(repo, Some(storage), cfg.id, row.entry_id.as_str()),
            binding_store::entry_bindings_complete_scoped(storage, repo, &scope),
        )
    } else {
        (false, false)
    };
    row.api_secret_present = secret_ok;
    row.bindings_complete = binding_ok;
}
