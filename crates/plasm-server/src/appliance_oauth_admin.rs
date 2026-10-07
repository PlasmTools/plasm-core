//! In-process outbound OAuth provider administration (DB + KV + catalog refresh), shared by TUI and CLI.

use std::sync::Arc;
use std::time::Duration;

use plasm_agent_core::mcp_config_repository::McpConfigRepository;
use plasm_agent_core::oauth_binding_kv::{oauth_binding_kv_key, write_oauth_binding_pointer};
use plasm_agent_core::oauth_link_catalog::OauthLinkCatalog;
use plasm_agent_core::oauth_provider_model::RuntimeOauthProviderMeta;
use plasm_agent_core::oauth_provider_repository;
use plasm_agent_core::oauth_runtime_source::{
    apply_runtime_source_to_catalog, PostgresOauthRuntimeProviderSource,
};
use plasm_agent_core::secret_store::SecretStore;
use plasm_runtime::{
    build_oauth_token_http_client, parse_outbound_oauth_kv_v1, poll_oauth_device_token_once,
    request_oauth_device_authorization, OAuthDeviceTokenPoll, OutboundOAuthKvV1,
};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("entry_id required")]
    EmptyEntryId,
    #[error("token_endpoint required")]
    EmptyTokenEndpoint,
    #[error(
        "device_authorization_endpoint missing for `{entry_id}` (upsert provider with device URL)"
    )]
    MissingDeviceEndpoint { entry_id: String },
    #[error("invalid provider metadata: {source}")]
    ProviderMetadata {
        #[source]
        source: plasm_agent_core::oauth_provider_model::MetaBuildError,
    },
    #[error("OAuth provider database operation failed: {source}")]
    ProviderDatabase {
        #[source]
        source: Box<plasm_agent_core::mcp_config_admin::McpConfigAdminError>,
    },
    #[error("OAuth catalog refresh failed: {source}")]
    ProviderRefresh {
        #[source]
        source: plasm_agent_core::oauth_runtime_source::OauthRuntimeFetchError,
    },
    #[error("OAuth provider resolution failed: {source}")]
    ProviderResolution {
        #[source]
        source: Box<plasm_agent_core::oauth_link_catalog::OauthResolveError>,
    },
    #[error("KV client secret write failed: {source}")]
    ClientSecretWrite {
        #[source]
        source: Box<plasm_agent_core::secret_store::SecretStoreError>,
    },
    #[error("OAuth HTTP client initialization failed: {source}")]
    HttpClient {
        #[source]
        source: plasm_runtime::RuntimeError,
    },
    #[error("device authorization request failed: {source}")]
    DeviceAuthorization {
        #[source]
        source: plasm_runtime::OAuthConnectError,
    },
    #[error("device token poll failed: {source}")]
    DevicePoll {
        #[source]
        source: plasm_runtime::OAuthConnectError,
    },
    #[error("device authorization timed out after {wait_secs}s")]
    DeviceTimedOut { wait_secs: u64 },
    #[error("device token error: {error} ({error_description:?})")]
    DeviceRejected {
        error: String,
        error_description: Option<String>,
    },
    #[error("OAuth token envelope rejected: {source}")]
    TokenEnvelope {
        #[source]
        source: plasm_runtime::ApplyTokenError,
    },
    #[error("OAuth token serialization failed: {source}")]
    TokenSerialization {
        #[source]
        source: serde_json::Error,
    },
    #[error("OAuth token write failed for `{key}`: {source}")]
    TokenWrite {
        key: String,
        #[source]
        source: Box<plasm_agent_core::secret_store::SecretStoreError>,
    },
    #[error("OAuth binding pointer write failed for `{entry_id}`: {source}")]
    BindingPointerWrite {
        entry_id: String,
        #[source]
        source: Box<plasm_agent_core::oauth_binding_kv::OAuthBindingWriteError>,
    },
    #[error("OAuth binding KV read failed for `{key}`: {source}")]
    BindingRead {
        key: String,
        #[source]
        source: Box<plasm_agent_core::secret_store::SecretStoreError>,
    },
    #[error("OAuth binding pointer corrupt for `{entry_id}`: {source}")]
    BindingPointerParse {
        entry_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("OAuth binding incomplete for `{entry_id}`: hosted_kv_key missing")]
    BindingPointerIncomplete { entry_id: String },
    #[error("OAuth token for `{entry_id}` has invalid UTF-8: {source}")]
    TokenEncoding {
        entry_id: String,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("OAuth token envelope invalid for `{entry_id}`: {source}")]
    TokenParse {
        entry_id: String,
        #[source]
        source: plasm_runtime::OutboundOAuthKvParseError,
    },
    #[error("auth storage initialization failed: {source}")]
    AuthInitialization {
        #[source]
        source: Box<plasm_agent_core::secret_store::SecretStoreError>,
    },
    #[error("appliance database initialization failed: {source}")]
    RepositoryInitialization {
        #[source]
        source: plasm_agent_core::mcp_config_repository::McpConfigRepositoryError,
    },
    #[error("client secret stdin read failed: {source}")]
    SecretInput {
        #[source]
        source: std::io::Error,
    },
    #[error("client secret key invalid: {source}")]
    ClientSecretKey {
        #[from]
        source: OauthClientSecretKeyError,
    },
    #[error("OAuth provider list serialization failed: {source}")]
    ProviderListSerialization {
        #[source]
        source: serde_json::Error,
    },
    #[error("MCP admin unavailable")]
    McpAdminUnavailable,
    #[error("runtime snapshot missing for MCP configuration `{config_id}`")]
    ConfigSnapshotMissing { config_id: uuid::Uuid },
    #[error("auth storage unavailable")]
    AuthStorageUnavailable,
    #[error("mcp config repo unavailable")]
    ConfigRepositoryUnavailable,
    #[error("OAuth catalog unavailable")]
    OAuthCatalogUnavailable,
    #[error("MCP administration failed: {source}")]
    McpAdministration {
        #[source]
        source: Box<plasm_agent_core::mcp_config_admin::McpConfigAdminError>,
    },
    #[error("outbound secret write failed for `{key}`: {source}")]
    OutboundSecretWrite {
        key: String,
        #[source]
        source: Box<plasm_agent_core::secret_store::SecretStoreError>,
    },
    #[error("binding values invalid for `{entry_id}`: {source}")]
    BindingValues {
        entry_id: String,
        #[source]
        source: plasm_agent_core::binding_slots::ConnectBindingError,
    },
    #[error("binding store failed for `{entry_id}`: {source}")]
    BindingStore {
        entry_id: String,
        #[source]
        source: Box<plasm_agent_core::binding_store::BindingStoreError>,
    },
}

/// Fixed KV location for this catalog `entry_id` (matches `plasm-server oauth provider upsert`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OauthClientSecretKeyError {
    #[error("entry_id must be non-empty")]
    EmptyEntryId,
    #[error(
        "OAuth client secret storage key exceeds 255 chars (len={length}); use a shorter entry_id"
    )]
    KeyTooLong { length: usize },
}

pub fn appliance_oauth_client_secret_kv_key(
    entry_id: &str,
) -> Result<String, OauthClientSecretKeyError> {
    const PREFIX: &str = "plasm:oauth_app:v1:";
    let e = entry_id.trim();
    if e.is_empty() {
        return Err(OauthClientSecretKeyError::EmptyEntryId);
    }
    let s = format!("{PREFIX}{e}");
    if s.len() > 255 {
        return Err(OauthClientSecretKeyError::KeyTooLong { length: s.len() });
    }
    Ok(s)
}

/// Disable a provider row in Postgres (when configured) and refresh the in-memory catalog.
pub async fn appliance_oauth_provider_disable(
    repo: Option<&McpConfigRepository>,
    catalog: &OauthLinkCatalog,
    entry_id: &str,
) -> Result<(), AdminError> {
    let entry_id = entry_id.trim();
    if entry_id.is_empty() {
        return Err(AdminError::EmptyEntryId);
    }
    if let Some(r) = repo {
        let n = oauth_provider_repository::set_oauth_provider_enabled(r.pool(), entry_id, false)
            .await
            .map_err(|source| AdminError::ProviderDatabase {
                source: Box::new(source.into()),
            })?;
        if n == 0 {
            catalog.remove_runtime(entry_id).await;
        }
        let src = PostgresOauthRuntimeProviderSource::new(r.pool().clone());
        apply_runtime_source_to_catalog(&src, catalog)
            .await
            .map_err(|source| AdminError::ProviderRefresh { source })?;
    } else {
        catalog.remove_runtime(entry_id).await;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ApplianceOauthUpsert {
    pub entry_id: String,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
    pub default_scopes: Vec<String>,
    pub client_id: String,
    pub client_secret_key: String,
    pub client_secret_value: Option<String>,
    pub enabled: bool,
}

pub async fn appliance_oauth_upsert_provider(
    repo: Option<&McpConfigRepository>,
    catalog: &OauthLinkCatalog,
    storage: &Arc<dyn SecretStore>,
    u: ApplianceOauthUpsert,
) -> Result<(), AdminError> {
    let entry_id = u.entry_id.trim();
    if entry_id.is_empty() {
        return Err(AdminError::EmptyEntryId);
    }
    let token_ep = u.token_endpoint.trim();
    if token_ep.is_empty() {
        return Err(AdminError::EmptyTokenEndpoint);
    }

    RuntimeOauthProviderMeta::try_from_parts(
        u.authorization_endpoint.as_deref(),
        token_ep,
        u.device_authorization_endpoint.as_deref(),
        u.default_scopes.clone(),
        u.client_id.trim(),
        u.client_secret_key.trim(),
    )
    .map_err(|source| AdminError::ProviderMetadata { source })?;

    if let Some(secret) = u
        .client_secret_value
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        storage
            .store_kv(u.client_secret_key.trim(), secret.as_bytes(), None)
            .await
            .map_err(|source| AdminError::ClientSecretWrite {
                source: Box::new(source),
            })?;
    }

    if u.enabled {
        if let Some(r) = repo {
            oauth_provider_repository::upsert_oauth_provider_app(
                r.pool(),
                oauth_provider_repository::UpsertOauthProviderParams {
                    entry_id,
                    authorization_endpoint: u.authorization_endpoint.as_deref(),
                    token_endpoint: token_ep,
                    device_authorization_endpoint: u.device_authorization_endpoint.as_deref(),
                    client_id: u.client_id.trim(),
                    client_secret_key: u.client_secret_key.trim(),
                    enabled: true,
                },
            )
            .await
            .map_err(|source| AdminError::ProviderDatabase {
                source: Box::new(source.into()),
            })?;
            let src = PostgresOauthRuntimeProviderSource::new(r.pool().clone());
            apply_runtime_source_to_catalog(&src, catalog)
                .await
                .map_err(|source| AdminError::ProviderRefresh { source })?;
        } else {
            let meta = RuntimeOauthProviderMeta::try_from_parts(
                u.authorization_endpoint.as_deref(),
                token_ep,
                u.device_authorization_endpoint.as_deref(),
                u.default_scopes.clone(),
                u.client_id.trim(),
                u.client_secret_key.trim(),
            )
            .map_err(|source| AdminError::ProviderMetadata { source })?;
            catalog.upsert_runtime(entry_id.to_string(), meta).await;
        }
    } else if let Some(r) = repo {
        let n = oauth_provider_repository::set_oauth_provider_enabled(r.pool(), entry_id, false)
            .await
            .map_err(|source| AdminError::ProviderDatabase {
                source: Box::new(source.into()),
            })?;
        if n == 0 {
            catalog.remove_runtime(entry_id).await;
        }
        let src = PostgresOauthRuntimeProviderSource::new(r.pool().clone());
        apply_runtime_source_to_catalog(&src, catalog)
            .await
            .map_err(|source| AdminError::ProviderRefresh { source })?;
    } else {
        catalog.remove_runtime(entry_id).await;
    }

    Ok(())
}

#[derive(Debug)]
pub struct OAuthBindingStatus {
    pub hint: String,
    pub bound: bool,
    pub warning: Option<AdminError>,
}

pub async fn oauth_binding_status(
    storage: &Arc<dyn SecretStore>,
    entry_id: &str,
) -> Result<OAuthBindingStatus, AdminError> {
    let key = oauth_binding_kv_key(entry_id);
    let Some(raw) =
        storage
            .get_kv(key.as_str())
            .await
            .map_err(|source| AdminError::BindingRead {
                key: key.clone(),
                source: Box::new(source),
            })?
    else {
        return Ok(OAuthBindingStatus {
            hint: "no binding".into(),
            bound: false,
            warning: None,
        });
    };
    let ptr = serde_json::from_slice::<serde_json::Value>(&raw).map_err(|source| {
        AdminError::BindingPointerParse {
            entry_id: entry_id.to_owned(),
            source,
        }
    })?;
    let hkv = ptr
        .get("hosted_kv_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AdminError::BindingPointerIncomplete {
            entry_id: entry_id.to_owned(),
        })?;
    let Some(tok) = storage
        .get_kv(hkv)
        .await
        .map_err(|source| AdminError::BindingRead {
            key: hkv.to_owned(),
            source: Box::new(source),
        })?
    else {
        return Ok(OAuthBindingStatus {
            hint: "token missing".into(),
            bound: false,
            warning: None,
        });
    };
    let utf8 = match std::str::from_utf8(&tok) {
        Ok(value) => value,
        Err(source) => {
            return Ok(OAuthBindingStatus {
                hint: "kv present".into(),
                bound: true,
                warning: Some(AdminError::TokenEncoding {
                    entry_id: entry_id.to_owned(),
                    source,
                }),
            })
        }
    };
    Ok(match parse_outbound_oauth_kv_v1(utf8) {
        Ok(env) => OAuthBindingStatus {
            hint: format!(
                "kv ok · exp {:?}",
                env.expires_at_unix
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "?".into())
            ),
            bound: true,
            warning: None,
        },
        Err(source) => OAuthBindingStatus {
            hint: "kv present".into(),
            bound: true,
            warning: Some(AdminError::TokenParse {
                entry_id: entry_id.to_owned(),
                source,
            }),
        },
    })
}

#[derive(Debug, Clone)]
pub struct DeviceBindPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in_secs: u64,
    pub poll_interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct DeviceBindOutcome {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in_secs: u64,
    pub poll_interval_secs: u64,
}

/// Device authorization + poll until token stored (RFC 8628).
pub async fn appliance_oauth_device_bind(
    entry_id: &str,
    catalog: &OauthLinkCatalog,
    storage: &Arc<dyn SecretStore>,
    scopes: &[String],
    max_wait: Duration,
    on_start: impl FnOnce(&DeviceBindPrompt),
) -> Result<DeviceBindOutcome, AdminError> {
    let entry_id = entry_id.trim();
    if entry_id.is_empty() {
        return Err(AdminError::EmptyEntryId);
    }

    let cfg = catalog
        .resolve_for_oauth_start(storage, entry_id)
        .await
        .map_err(|source| AdminError::ProviderResolution {
            source: Box::new(source),
        })?;

    let device_url = cfg
        .device_authorization_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s: &&str| !s.is_empty())
        .ok_or_else(|| AdminError::MissingDeviceEndpoint {
            entry_id: entry_id.to_owned(),
        })?;

    let http_timeout = Duration::from_secs(30);
    let http = build_oauth_token_http_client(http_timeout)
        .map_err(|source| AdminError::HttpClient { source })?;

    let start = request_oauth_device_authorization(
        &http,
        device_url,
        cfg.client_id.trim(),
        Some(cfg.client_secret.as_str()),
        scopes,
        http_timeout,
    )
    .await
    .map_err(|source| AdminError::DeviceAuthorization { source })?;

    let mut interval = Duration::from_secs(start.interval.unwrap_or(5).max(1));
    let prompt = DeviceBindPrompt {
        user_code: start.user_code.clone(),
        verification_uri: start.verification_uri.clone(),
        verification_uri_complete: start.verification_uri_complete.clone(),
        expires_in_secs: start.expires_in,
        poll_interval_secs: interval.as_secs(),
    };
    on_start(&prompt);
    let deadline = tokio::time::Instant::now() + max_wait;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(AdminError::DeviceTimedOut {
                wait_secs: max_wait.as_secs(),
            });
        }

        match poll_oauth_device_token_once(
            &http,
            cfg.token_endpoint.trim(),
            cfg.client_id.trim(),
            Some(cfg.client_secret.as_str()),
            start.device_code.trim(),
            http_timeout,
        )
        .await
        .map_err(|source| AdminError::DevicePoll { source })?
        {
            OAuthDeviceTokenPoll::Success(token_json) => {
                let envelope =
                    OutboundOAuthKvV1::from_token_json_for_entry(entry_id.to_string(), &token_json)
                        .map_err(|source| AdminError::TokenEnvelope { source })?;
                let hosted_kv_key = format!("plasm:outbound:v1:{}", uuid::Uuid::new_v4());
                let envelope_bytes = serde_json::to_vec(&envelope)
                    .map_err(|source| AdminError::TokenSerialization { source })?;
                storage
                    .store_kv(&hosted_kv_key, &envelope_bytes, None)
                    .await
                    .map_err(|source| AdminError::TokenWrite {
                        key: hosted_kv_key.clone(),
                        source: Box::new(source),
                    })?;
                write_oauth_binding_pointer(storage, entry_id, &hosted_kv_key)
                    .await
                    .map_err(|source| AdminError::BindingPointerWrite {
                        entry_id: entry_id.to_owned(),
                        source: Box::new(source),
                    })?;
                return Ok(DeviceBindOutcome {
                    user_code: prompt.user_code.clone(),
                    verification_uri: prompt.verification_uri.clone(),
                    verification_uri_complete: prompt.verification_uri_complete.clone(),
                    expires_in_secs: prompt.expires_in_secs,
                    poll_interval_secs: interval.as_secs(),
                });
            }
            OAuthDeviceTokenPoll::AuthorizationPending => {
                tokio::time::sleep(interval).await;
            }
            OAuthDeviceTokenPoll::SlowDown { interval_secs } => {
                interval = Duration::from_secs(interval_secs.max(1));
                tokio::time::sleep(interval).await;
            }
            OAuthDeviceTokenPoll::OAuthError {
                error,
                error_description,
            } => {
                return Err(AdminError::DeviceRejected {
                    error,
                    error_description,
                });
            }
        }
    }
}

#[cfg(test)]
mod admin_error_tests {
    use super::*;
    use plasm_agent_core::secret_store::MemorySecretStore;
    use std::error::Error;

    #[test]
    fn admin_error_preserves_concrete_sources_and_is_channel_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AdminError>();
        let size = std::mem::size_of::<AdminError>();
        assert!(size < 128, "AdminError occupies {size} bytes");
        let error = AdminError::ProviderResolution {
            source: Box::new(plasm_agent_core::oauth_link_catalog::OauthResolveError::UnknownEntry),
        };
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<Box<plasm_agent_core::oauth_link_catalog::OauthResolveError>>()
                .map(Box::as_ref),
            Some(plasm_agent_core::oauth_link_catalog::OauthResolveError::UnknownEntry)
        ));
        let error = AdminError::SecretInput {
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn boxed_auth_write_preserves_key_display_and_nested_storage_source() {
        let source = plasm_agent_core::secret_store::SecretStoreError::BackendUnavailable;
        let display = format!("OAuth token write failed for `fixture-key`: {source}");
        let error = AdminError::TokenWrite {
            key: "fixture-key".into(),
            source: Box::new(source),
        };
        assert_eq!(error.to_string(), display);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<plasm_agent_core::secret_store::SecretStoreError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(
            source,
            plasm_agent_core::secret_store::SecretStoreError::BackendUnavailable
        ));
        assert!(source.source().is_none());
        let AdminError::TokenWrite { key, source } = error else {
            panic!("expected token write error");
        };
        assert_eq!(key, "fixture-key");
        assert!(matches!(
            *source,
            plasm_agent_core::secret_store::SecretStoreError::BackendUnavailable
        ));
    }

    #[test]
    fn boxed_binding_store_preserves_entry_and_concrete_cause_chain() {
        use plasm_agent_core::binding_store::BindingStoreError;
        let source = BindingStoreError::KvStore {
            source: plasm_agent_core::secret_store::SecretStoreError::BackendUnavailable,
        };
        let display = format!("binding store failed for `fixture`: {source}");
        let error = AdminError::BindingStore {
            entry_id: "fixture".into(),
            source: Box::new(source),
        };
        assert_eq!(error.to_string(), display);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<BindingStoreError>>()
            .unwrap()
            .as_ref();
        let auth = source
            .source()
            .unwrap()
            .downcast_ref::<plasm_agent_core::secret_store::SecretStoreError>()
            .unwrap();
        assert!(auth.source().is_none());
        let AdminError::BindingStore { entry_id, source } = error else {
            panic!("expected binding store error");
        };
        assert_eq!(entry_id, "fixture");
        assert!(matches!(*source, BindingStoreError::KvStore { .. }));
    }

    #[tokio::test]
    async fn absent_binding_is_data_but_corrupt_pointer_preserves_parse_cause() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let status = oauth_binding_status(&storage, "matrix").await.unwrap();
        assert!(!status.bound);
        assert!(status.warning.is_none());
        storage
            .store_kv(&oauth_binding_kv_key("matrix"), b"{", None)
            .await
            .unwrap();
        let error = oauth_binding_status(&storage, "matrix").await.unwrap_err();
        assert!(
            matches!(&error, AdminError::BindingPointerParse { entry_id, .. } if entry_id == "matrix")
        );
        assert!(error
            .source()
            .unwrap()
            .downcast_ref::<serde_json::Error>()
            .is_some());
    }

    #[tokio::test]
    async fn incomplete_pointer_is_semantic_and_invalid_token_keeps_bound_status() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        storage
            .store_kv(&oauth_binding_kv_key("matrix"), b"{}", None)
            .await
            .unwrap();
        assert!(
            matches!(oauth_binding_status(&storage, "matrix").await, Err(AdminError::BindingPointerIncomplete { entry_id }) if entry_id == "matrix")
        );
        let key = "plasm:outbound:v1:matrix";
        storage.store_kv(key, b"not-json", None).await.unwrap();
        write_oauth_binding_pointer(&storage, "matrix", key)
            .await
            .unwrap();
        let status = oauth_binding_status(&storage, "matrix").await.unwrap();
        assert!(status.bound);
        assert!(matches!(
            status.warning,
            Some(AdminError::TokenParse { .. })
        ));
    }
}
