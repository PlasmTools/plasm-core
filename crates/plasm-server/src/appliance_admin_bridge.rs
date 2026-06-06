//! Tokio-hosted MCP/OAuth admin router: UI thread enqueues [`AdminJob`] via [`mpsc::UnboundedSender`];
//! completions are polled with [`crossbeam_channel::Receiver::try_recv`] (never blocking `recv`).
//!
//! Jobs run **serially** on one async task so list refreshes and key provisioning cannot pile up
//! concurrent sqlx work against the same pool (which previously left the footer stuck on “Refreshing…”).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use auth_framework::storage::AuthStorage;
use plasm_agent_core::mcp_api_key_registry::McpApiKeyProvisioned;
use plasm_agent_core::mcp_config_admin::{
    McpCatalogAuthMarker, McpConfigAdminService, McpConfigApiKeyRow, McpConfigCatalogRow,
};
use plasm_agent_core::oauth_link_catalog::OauthLinkCatalog;
use plasm_agent_core::oauth_provider_repository::OauthProviderAppRow;
use plasm_agent_core::server_state::PlasmHostState;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::appliance_mcp_admin::{
    admin_service_from_host, appliance_mcp_scope, appliance_preferred_config_id,
};
use crate::appliance_oauth_admin::{self, ApplianceOauthUpsert};

pub type AdminCorr = u64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PolicyStoreUnavailableReason {
    #[default]
    NeverAttached,
    #[allow(dead_code)] // first admin refresh in flight (see RUN overview copy)
    RefreshPending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpConfigSurfaceState {
    PolicyStoreUnavailable {
        reason: PolicyStoreUnavailableReason,
    },
    ConfigLoadError,
    Ready {
        summary_name: String,
        summary_status: String,
        enabled_api_count: usize,
        key_count: usize,
    },
}

impl Default for McpConfigSurfaceState {
    fn default() -> Self {
        Self::PolicyStoreUnavailable {
            reason: PolicyStoreUnavailableReason::NeverAttached,
        }
    }
}

/// Seed RUN-mode Overview before the first admin refresh completes.
pub fn config_surface_from_host(state: &PlasmHostState) -> McpConfigSurfaceState {
    if state.mcp_config_repository().is_some() {
        McpConfigSurfaceState::Ready {
            summary_name: "Your MCP".into(),
            summary_status: "attached".into(),
            enabled_api_count: 0,
            key_count: 0,
        }
    } else {
        McpConfigSurfaceState::PolicyStoreUnavailable {
            reason: PolicyStoreUnavailableReason::NeverAttached,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OAuthSurfaceState {
    #[default]
    CatalogUnavailable,
    AuthStorageUnavailable,
    ProviderStoreUnavailable,
    ProviderListUnavailable(String),
    Ready,
}

impl OAuthSurfaceState {
    pub fn services_ready(&self) -> bool {
        matches!(
            self,
            Self::ProviderStoreUnavailable | Self::ProviderListUnavailable(_) | Self::Ready
        )
    }

    pub fn provider_store_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn status_message(&self) -> Option<&str> {
        match self {
            Self::CatalogUnavailable => Some("OAuth catalog unavailable"),
            Self::AuthStorageUnavailable => Some("OAuth auth storage unavailable"),
            Self::ProviderStoreUnavailable => {
                Some("OAuth provider store unavailable (policy DB not attached)")
            }
            Self::ProviderListUnavailable(msg) => Some(msg.as_str()),
            Self::Ready => None,
        }
    }
}

/// Snapshot payload for the RUN-mode UI (mirrors [`crate::tui::UiSnapshot`] fields).
#[derive(Default)]
pub struct RefreshedUiData {
    pub config_surface: McpConfigSurfaceState,
    pub config_id: Option<Uuid>,
    pub catalog_rows: Vec<McpConfigCatalogRow>,
    pub keys: Vec<McpConfigApiKeyRow>,
    pub db_allowed: HashSet<String>,
    pub oauth_providers: Vec<OauthProviderAppRow>,
    pub oauth_binding_hints: Vec<String>,
    pub oauth_surface: OAuthSurfaceState,
}

pub enum AdminJob {
    RefreshFull {
        corr: AdminCorr,
    },
    ProvisionApiKey {
        corr: AdminCorr,
        config_id: Uuid,
        label: String,
    },
    SetAllowedApisExact {
        corr: AdminCorr,
        config_id: Uuid,
        entry_ids: HashSet<String>,
    },
    StoreOutboundSecret {
        corr: AdminCorr,
        key: String,
        value: String,
    },
    StoreMcpCatalogBinding {
        corr: AdminCorr,
        tenant_id: String,
        config_id: Uuid,
        entry_id: String,
        values: std::collections::HashMap<String, String>,
    },
    OAuthDeviceBind {
        corr: AdminCorr,
        entry_id: String,
        /// When empty, the admin job uses provider `default_scopes` from `resolve_for_oauth_start`.
        /// The control-station OAuth tab sends explicit scopes whenever CGS defines `oauth.scopes`.
        scopes: Vec<String>,
        catalog: Arc<OauthLinkCatalog>,
        storage: Arc<dyn AuthStorage>,
    },
    OauthProviderUpsert {
        corr: AdminCorr,
        upsert: ApplianceOauthUpsert,
    },
    OauthProviderDisable {
        corr: AdminCorr,
        entry_id: String,
    },
    RotateApiKey {
        corr: AdminCorr,
        config_id: Uuid,
        key_id: Uuid,
    },
    RevokeApiKey {
        corr: AdminCorr,
        config_id: Uuid,
        key_id: Uuid,
    },
    RevealApiKey {
        corr: AdminCorr,
        config_id: Uuid,
        key_id: Uuid,
    },
}

pub enum AdminCompletion {
    RefreshFull {
        corr: AdminCorr,
        data: RefreshedUiData,
    },
    ProvisionApiKey {
        corr: AdminCorr,
        result: Result<McpApiKeyProvisioned, String>,
    },
    SetAllowedApisExact {
        corr: AdminCorr,
        result: Result<(), String>,
    },
    StoreOutboundSecret {
        corr: AdminCorr,
        key: String,
        result: Result<(), String>,
    },
    StoreMcpCatalogBinding {
        corr: AdminCorr,
        entry_id: String,
        result: Result<(), String>,
    },
    OAuthDeviceBindStarted {
        corr: AdminCorr,
        prompt: appliance_oauth_admin::DeviceBindPrompt,
    },
    OAuthDeviceBind {
        corr: AdminCorr,
        result: Result<appliance_oauth_admin::DeviceBindOutcome, String>,
    },
    OauthProviderUpsert {
        corr: AdminCorr,
        result: Result<(), String>,
    },
    OauthProviderDisable {
        corr: AdminCorr,
        result: Result<(), String>,
    },
    RotateApiKey {
        corr: AdminCorr,
        result: Result<McpApiKeyProvisioned, String>,
    },
    RevokeApiKey {
        corr: AdminCorr,
        result: Result<(), String>,
    },
    RevealApiKey {
        corr: AdminCorr,
        result: Result<String, String>,
    },
}

pub struct AdminBridge {
    pub jobs_tx: mpsc::UnboundedSender<AdminJob>,
    completions_rx: crossbeam_channel::Receiver<AdminCompletion>,
}

impl AdminBridge {
    #[inline]
    pub fn completions(&self) -> &crossbeam_channel::Receiver<AdminCompletion> {
        &self.completions_rx
    }
}

fn send_completion(tx: &crossbeam_channel::Sender<AdminCompletion>, msg: AdminCompletion) {
    if tx.send(msg).is_err() {
        tracing::warn!(
            target: "plasm_appliance_admin",
            "admin completion dropped (UI thread ended)"
        );
    }
}

fn rotated_api_key_label(rows: Vec<McpConfigApiKeyRow>, key_id: Uuid) -> String {
    rows.into_iter()
        .find(|row| row.key_id == key_id)
        .and_then(|row| row.label)
        .unwrap_or_default()
}

fn apply_oauth_binding_state_to_catalog_rows(
    rows: &mut [McpConfigCatalogRow],
    bound_entry_ids: &HashSet<String>,
) {
    for row in rows {
        if !bound_entry_ids.contains(&row.entry_id) {
            continue;
        }
        row.has_auth_binding = true;
        if matches!(row.auth_marker, McpCatalogAuthMarker::MissingBinding) {
            row.auth_marker = McpCatalogAuthMarker::RequiresConnect;
        }
    }
}

async fn apply_local_secret_state_to_catalog_rows(
    rows: &mut [McpConfigCatalogRow],
    storage: &Arc<dyn AuthStorage>,
) {
    for row in rows {
        let Some(key) = row.api_secret_hosted_kv.as_deref() else {
            row.api_secret_present = false;
            continue;
        };
        row.api_secret_present = matches!(storage.get_kv(key).await, Ok(Some(_)));
    }
}

async fn apply_binding_state_to_catalog_rows(
    rows: &mut [McpConfigCatalogRow],
    storage: &Arc<dyn AuthStorage>,
    repo: &plasm_agent_core::mcp_config_repository::McpConfigRepository,
    config_id: uuid::Uuid,
    tenant_id: &str,
) {
    for row in rows {
        if !row.bindings_required {
            row.bindings_complete = true;
            continue;
        }
        let scope = plasm_agent_core::binding_slots::BindingScope::new(
            tenant_id,
            config_id,
            row.entry_id.clone(),
        );
        row.bindings_complete =
            plasm_agent_core::binding_store::entry_bindings_complete_scoped(
                storage,
                repo,
                &scope,
            )
            .await;
    }
}

async fn refresh_oauth_into(state: &PlasmHostState, data: &mut RefreshedUiData) {
    data.oauth_providers.clear();
    data.oauth_binding_hints.clear();
    let Some(_catalog) = state.oauth_link_catalog() else {
        data.oauth_surface = OAuthSurfaceState::CatalogUnavailable;
        return;
    };
    let Some(storage) = state.auth_storage() else {
        data.oauth_surface = OAuthSurfaceState::AuthStorageUnavailable;
        return;
    };
    let Some(repo) = state.mcp_config_repository() else {
        data.oauth_surface = OAuthSurfaceState::ProviderStoreUnavailable;
        return;
    };
    let pool = repo.pool().clone();
    let storage = Arc::clone(storage);
    let rows =
        match plasm_agent_core::oauth_provider_repository::list_oauth_provider_apps(&pool).await {
            Ok(rows) => rows,
            Err(e) => {
                data.oauth_surface = OAuthSurfaceState::ProviderListUnavailable(format!(
                    "OAuth provider list unavailable: {e}"
                ));
                return;
            }
        };
    let mut hints = Vec::with_capacity(rows.len());
    let mut bound_entry_ids = HashSet::new();
    for r in &rows {
        let status = appliance_oauth_admin::oauth_binding_status(&storage, &r.entry_id).await;
        if status.bound {
            bound_entry_ids.insert(r.entry_id.clone());
        }
        hints.push(status.hint);
    }
    apply_local_secret_state_to_catalog_rows(&mut data.catalog_rows, &storage).await;
    apply_oauth_binding_state_to_catalog_rows(&mut data.catalog_rows, &bound_entry_ids);
    data.oauth_surface = OAuthSurfaceState::Ready;
    data.oauth_providers = rows;
    data.oauth_binding_hints = hints;
}

pub async fn refresh_full_snapshot(state: &PlasmHostState) -> RefreshedUiData {
    let mut data = RefreshedUiData::default();

    let Some(admin) = admin_service_from_host(state) else {
        data.config_id = None;
        refresh_oauth_into(state, &mut data).await;
        return data;
    };

    let scope = appliance_mcp_scope();
    let pref = appliance_preferred_config_id();
    let reg = state.catalog.snapshot();
    let admin = admin.clone();

    let res = async move {
        let id = admin
            .ensure_singleton_config(&scope, pref, "Your MCP")
            .await
            .ok()?;
        let summary = admin.admin_summary(id).await.ok()?;
        let runtime = admin.load_runtime_snapshot(id).await.ok()??;
        let optional = admin.load_auth_optional_set(id).await.ok()?;
        let catalog_rows = McpConfigAdminService::catalog_rows(reg.as_ref(), &runtime, &optional);
        let keys = admin.list_api_key_rows(id).await.unwrap_or_default();
        Some((id, summary, runtime, catalog_rows, keys))
    }
    .await;

    let Some((id, summary, runtime, catalog_rows, keys)) = res else {
        data.config_surface = McpConfigSurfaceState::ConfigLoadError;
        refresh_oauth_into(state, &mut data).await;
        return data;
    };

    data.config_id = Some(id);
    data.config_surface = McpConfigSurfaceState::Ready {
        summary_name: summary.name,
        summary_status: summary.status,
        enabled_api_count: summary.enabled_api_count,
        key_count: keys.len(),
    };
    data.catalog_rows = catalog_rows;
    data.keys = keys;
    data.db_allowed = runtime.allowed_entry_ids.clone();
    if let (Some(storage), Some(repo)) = (state.auth_storage(), state.mcp_config_repository()) {
        apply_local_secret_state_to_catalog_rows(&mut data.catalog_rows, storage).await;
        apply_binding_state_to_catalog_rows(
            &mut data.catalog_rows,
            storage,
            repo,
            id,
            runtime.tenant_id.as_str(),
        )
        .await;
    }
    refresh_oauth_into(state, &mut data).await;
    data
}

async fn run_admin_job(
    state: Arc<PlasmHostState>,
    comp_tx: crossbeam_channel::Sender<AdminCompletion>,
    job: AdminJob,
) {
    match job {
        AdminJob::RefreshFull { corr } => {
            let snapshot_data = refresh_full_snapshot(state.as_ref()).await;
            send_completion(
                &comp_tx,
                AdminCompletion::RefreshFull {
                    corr,
                    data: snapshot_data,
                },
            );
        }
        AdminJob::ProvisionApiKey {
            corr,
            config_id,
            label,
        } => {
            let result = if let Some(admin) = admin_service_from_host(state.as_ref()) {
                admin
                    .provision_api_key(config_id, label)
                    .await
                    .map_err(|e| format!("{e}"))
            } else {
                Err("MCP admin unavailable".into())
            };
            send_completion(&comp_tx, AdminCompletion::ProvisionApiKey { corr, result });
        }
        AdminJob::SetAllowedApisExact {
            corr,
            config_id,
            entry_ids,
        } => {
            let result = if let Some(admin) = admin_service_from_host(state.as_ref()) {
                admin
                    .set_allowed_apis_exact(config_id, entry_ids)
                    .await
                    .map_err(|e| format!("{e}"))
            } else {
                Err("MCP admin unavailable".into())
            };
            send_completion(
                &comp_tx,
                AdminCompletion::SetAllowedApisExact { corr, result },
            );
        }
        AdminJob::StoreOutboundSecret { corr, key, value } => {
            let result = if let Some(storage) = state.auth_storage() {
                storage
                    .store_kv(key.as_str(), value.as_bytes(), None)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Err("auth storage unavailable".into())
            };
            send_completion(
                &comp_tx,
                AdminCompletion::StoreOutboundSecret { corr, key, result },
            );
        }
        AdminJob::StoreMcpCatalogBinding {
            corr,
            tenant_id,
            config_id,
            entry_id,
            values,
        } => {
            let result = async {
                let repo = state
                    .mcp_config_repository()
                    .ok_or_else(|| "mcp config repo unavailable".to_string())?;
                let storage = state
                    .auth_storage()
                    .ok_or_else(|| "auth storage unavailable".to_string())?;
                let normalized = plasm_agent_core::binding_slots::normalize_connect_binding_values(
                    entry_id.as_str(),
                    &values,
                )
                .map_err(|e| e.to_string())?;
                let scope = plasm_agent_core::binding_slots::BindingScope::new(
                    tenant_id,
                    config_id,
                    entry_id.clone(),
                );
                plasm_agent_core::binding_store::store_scoped_binding_envelope(
                    storage,
                    repo,
                    scope,
                    normalized,
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(())
            }
            .await;
            send_completion(
                &comp_tx,
                AdminCompletion::StoreMcpCatalogBinding {
                    corr,
                    entry_id,
                    result,
                },
            );
        }
        AdminJob::OAuthDeviceBind {
            corr,
            entry_id,
            scopes,
            catalog,
            storage,
        } => {
            let resolved_scopes: Vec<String> = if scopes.is_empty() {
                catalog
                    .resolve_for_oauth_start(&storage, entry_id.trim())
                    .await
                    .map(|c| c.default_scopes)
                    .unwrap_or_default()
            } else {
                scopes
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            let result = appliance_oauth_admin::appliance_oauth_device_bind(
                &entry_id,
                catalog.as_ref(),
                &storage,
                &resolved_scopes,
                Duration::from_secs(600),
                |prompt| {
                    send_completion(
                        &comp_tx,
                        AdminCompletion::OAuthDeviceBindStarted {
                            corr,
                            prompt: prompt.clone(),
                        },
                    );
                },
            )
            .await
            .map_err(|e| format!("{e}"));
            send_completion(&comp_tx, AdminCompletion::OAuthDeviceBind { corr, result });
        }
        AdminJob::OauthProviderUpsert { corr, upsert } => {
            let result = async {
                let catalog = state
                    .oauth_link_catalog()
                    .ok_or_else(|| "OAuth catalog unavailable".to_string())?;
                let storage = state
                    .auth_storage()
                    .ok_or_else(|| "auth storage unavailable".to_string())?;
                let repo = state.mcp_config_repository().map(|r| r.as_ref());
                appliance_oauth_admin::appliance_oauth_upsert_provider(
                    repo,
                    catalog.as_ref(),
                    storage,
                    upsert,
                )
                .await
                .map_err(|e| format!("{e:#}"))
            }
            .await;
            send_completion(
                &comp_tx,
                AdminCompletion::OauthProviderUpsert { corr, result },
            );
        }
        AdminJob::OauthProviderDisable { corr, entry_id } => {
            let result = async {
                let catalog = state
                    .oauth_link_catalog()
                    .ok_or_else(|| "OAuth catalog unavailable".to_string())?;
                let repo = state.mcp_config_repository().map(|r| r.as_ref());
                appliance_oauth_admin::appliance_oauth_provider_disable(
                    repo,
                    catalog.as_ref(),
                    &entry_id,
                )
                .await
                .map_err(|e| format!("{e:#}"))
            }
            .await;
            send_completion(
                &comp_tx,
                AdminCompletion::OauthProviderDisable { corr, result },
            );
        }
        AdminJob::RotateApiKey {
            corr,
            config_id,
            key_id,
        } => {
            let result = if let Some(admin) = admin_service_from_host(state.as_ref()) {
                let new_label = match admin.list_api_key_rows(config_id).await {
                    Ok(rows) => rotated_api_key_label(rows, key_id),
                    Err(e) => {
                        send_completion(
                            &comp_tx,
                            AdminCompletion::RotateApiKey {
                                corr,
                                result: Err(format!("{e}")),
                            },
                        );
                        return;
                    }
                };
                admin
                    .rotate_one_api_key(config_id, key_id, new_label)
                    .await
                    .map_err(|e| format!("{e}"))
            } else {
                Err("MCP admin unavailable".into())
            };
            send_completion(&comp_tx, AdminCompletion::RotateApiKey { corr, result });
        }
        AdminJob::RevokeApiKey {
            corr,
            config_id,
            key_id,
        } => {
            let result = if let Some(admin) = admin_service_from_host(state.as_ref()) {
                admin
                    .revoke_one_api_key(config_id, key_id)
                    .await
                    .map_err(|e| format!("{e}"))
            } else {
                Err("MCP admin unavailable".into())
            };
            send_completion(&comp_tx, AdminCompletion::RevokeApiKey { corr, result });
        }
        AdminJob::RevealApiKey {
            corr,
            config_id,
            key_id,
        } => {
            let result = if let Some(admin) = admin_service_from_host(state.as_ref()) {
                admin
                    .reveal_api_key(config_id, key_id)
                    .await
                    .map_err(|e| format!("{e}"))
            } else {
                Err("MCP admin unavailable".into())
            };
            send_completion(&comp_tx, AdminCompletion::RevealApiKey { corr, result });
        }
    }
}

pub fn spawn_admin_router(state: Arc<PlasmHostState>) -> AdminBridge {
    let (jobs_tx, mut jobs_rx) = mpsc::unbounded_channel();
    let (comp_tx, comp_rx) = crossbeam_channel::unbounded();

    // Run jobs **one at a time**. Nesting `tokio::spawn` per job allowed many concurrent
    // `RefreshFull` snapshots + `ProvisionApiKey` against the same sqlx pool; under load the UI
    // could sit forever with `pending_refresh_corr` set ("Refreshing…") while completions never
    // matched or work starved.
    tokio::spawn(async move {
        while let Some(job) = jobs_rx.recv().await {
            let state = Arc::clone(&state);
            let comp_tx = comp_tx.clone();
            run_admin_job(state, comp_tx, job).await;
        }
    });

    AdminBridge {
        jobs_tx,
        completions_rx: comp_rx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rotated_api_key_label_preserves_existing_label() {
        let key_id = Uuid::new_v4();
        let rows = vec![McpConfigApiKeyRow {
            key_id,
            fingerprint: "fp".into(),
            label: Some("bob".into()),
        }];

        assert_eq!(rotated_api_key_label(rows, key_id), "bob");
    }

    #[test]
    fn rotated_api_key_label_keeps_unnamed_keys_unnamed() {
        let key_id = Uuid::new_v4();
        let rows = vec![McpConfigApiKeyRow {
            key_id,
            fingerprint: "fp".into(),
            label: None,
        }];

        assert_eq!(rotated_api_key_label(rows, key_id), "");
    }

    #[test]
    fn default_config_surface_is_never_attached() {
        assert!(matches!(
            McpConfigSurfaceState::default(),
            McpConfigSurfaceState::PolicyStoreUnavailable {
                reason: PolicyStoreUnavailableReason::NeverAttached
            }
        ));
    }

    #[test]
    fn oauth_binding_state_updates_missing_binding_catalog_rows() {
        let mut rows: Vec<McpConfigCatalogRow> = vec![serde_json::from_value(json!({
            "entry_id": "github",
            "label": "GitHub",
            "enabled_for_mcp": true,
            "auth_optional": false,
            "has_auth_binding": false,
            "auth_marker": "missing_binding",
            "connect_profile": {
                "capability": "oauth_only",
                "oauth": { "provider_present": true, "scope_catalog_present": true },
                "has_public_mode": false,
                "has_api_key": false,
                "has_oauth": true
            }
        }))
        .expect("catalog row json")];
        let bound = HashSet::from(["github".to_string()]);

        apply_oauth_binding_state_to_catalog_rows(&mut rows, &bound);

        assert!(rows[0].has_auth_binding);
        assert_eq!(rows[0].auth_marker, McpCatalogAuthMarker::RequiresConnect);
    }
}
