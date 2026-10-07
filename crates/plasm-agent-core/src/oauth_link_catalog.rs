//! OAuth link provider catalog (`PLASM_OAUTH_LINK_PROVIDERS_PATH` JSON). Keys are registry `entry_id` values.
//!
//! Related env: `PLASM_OAUTH_LINK_REDIRECT_URI` (must match the OAuth app’s registered callback),
//! `PLASM_OAUTH_LINK_ALLOWED_RETURN_PREFIXES` (comma-separated allowlist for SaaS return URLs),
//! and per-provider `client_id_env` / `client_secret_env` names in the JSON file.
//! See `fixtures/oauth_link_providers.example.json`.
//!
//! **Runtime** rows override static env/file entries for the same `entry_id`. They are loaded from
//! Postgres table `public.oauth_provider_apps` (enabled rows) on startup and on a periodic refresh
//! when `PLASM_AUTH_STORAGE_URL` / `DATABASE_URL` / `PLASM_OAUTH_PROVIDER_DATABASE_URL` is set — see
//! `oauth_provider_pull`. `POST /internal/oauth-link/v1/provider-upsert` still applies immediate
//! in-memory updates until the next DB refresh. Client secrets are resolved from Plasm credential KV at
//! OAuth start using `client_secret_key` (JIT), never embedded in the in-memory catalog.

use crate::secret_store::SecretStore;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub use crate::oauth_provider_model::{
    MetaBuildError, OAuthEndpointUrl, OauthClientSecretKvRef, RuntimeOauthProviderMeta,
};

/// Loaded from `PLASM_OAUTH_LINK_PROVIDERS_PATH` (JSON). Keys are registry `entry_id` values.
#[derive(Clone, Debug)]
pub struct OauthLinkCatalog {
    pub redirect_uri: String,
    static_providers: HashMap<String, OauthProviderConfig>,
    allowed_return_prefixes: Vec<String>,
    /// Control-plane upserts; secrets live in KV at `client_secret_key`, resolved per start.
    runtime_providers: Arc<RwLock<HashMap<String, RuntimeOauthProviderMeta>>>,
}

/// Static catalog entry: client secret inlined from env at process start.
#[derive(Clone, Debug)]
pub(crate) struct OauthProviderConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[allow(dead_code)]
    pub device_authorization_endpoint: Option<String>,
    pub default_scopes: Vec<String>,
    pub client_id: String,
    pub client_secret: String,
}

/// Fully resolved provider for starting OAuth (secret is always materialized here for static or JIT).
#[derive(Clone, Debug)]
pub struct OauthResolvedProvider {
    /// Browser authorization-code URL when configured.
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: String,
    /// RFC 8628 device authorization endpoint when configured.
    pub device_authorization_endpoint: Option<String>,
    pub default_scopes: Vec<String>,
    pub client_id: String,
    pub client_secret: String,
}

impl Default for OauthLinkCatalog {
    fn default() -> Self {
        Self {
            redirect_uri: normalize_plasm_oauth_redirect_uri(
                "http://127.0.0.1:3001/oauth/link/callback",
            ),
            static_providers: HashMap::new(),
            allowed_return_prefixes: parse_prefix_list(
                "http://127.0.0.1:4000,http://localhost:4000",
            ),
            runtime_providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    providers: HashMap<String, CatalogProviderEntry>,
}

#[derive(Debug, Deserialize)]
struct CatalogProviderEntry {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    #[serde(default)]
    default_scopes: Vec<String>,
    client_id_env: String,
    client_secret_env: String,
}

impl OauthLinkCatalog {
    pub fn from_env() -> Self {
        let allowed_return_prefixes = parse_prefix_list(
            &std::env::var("PLASM_OAUTH_LINK_ALLOWED_RETURN_PREFIXES")
                .unwrap_or_else(|_| "http://127.0.0.1:4000,http://localhost:4000".into()),
        );
        let redirect_uri = normalize_plasm_oauth_redirect_uri(
            std::env::var("PLASM_OAUTH_LINK_REDIRECT_URI")
                .unwrap_or_else(|_| "http://127.0.0.1:3001/oauth/link/callback".into()),
        );
        let path = match std::env::var("PLASM_OAUTH_LINK_PROVIDERS_PATH") {
            Ok(p) if !p.trim().is_empty() => p,
            _ => {
                tracing::info!(
                    "PLASM_OAUTH_LINK_PROVIDERS_PATH unset; OAuth link catalog is empty"
                );
                return Self {
                    redirect_uri,
                    static_providers: HashMap::new(),
                    allowed_return_prefixes,
                    runtime_providers: Arc::new(RwLock::new(HashMap::new())),
                };
            }
        };
        let data = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "oauth link catalog file read failed");
                return Self {
                    redirect_uri,
                    static_providers: HashMap::new(),
                    allowed_return_prefixes,
                    runtime_providers: Arc::new(RwLock::new(HashMap::new())),
                };
            }
        };
        let parsed: CatalogFile = match serde_json::from_str(&data) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "oauth link catalog JSON invalid");
                return Self {
                    redirect_uri,
                    static_providers: HashMap::new(),
                    allowed_return_prefixes,
                    runtime_providers: Arc::new(RwLock::new(HashMap::new())),
                };
            }
        };
        let redirect_uri = normalize_plasm_oauth_redirect_uri(
            parsed
                .redirect_uri
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(redirect_uri),
        );
        let mut static_providers = HashMap::new();
        for (entry_id, ent) in parsed.providers {
            let client_id = match std::env::var(&ent.client_id_env) {
                Ok(v) if !v.trim().is_empty() => v,
                _ => {
                    tracing::warn!(
                        entry_id = %entry_id,
                        env = %ent.client_id_env,
                        "oauth link provider skipped (missing client id env)"
                    );
                    continue;
                }
            };
            let client_secret = match std::env::var(&ent.client_secret_env) {
                Ok(v) if !v.trim().is_empty() => v,
                _ => {
                    tracing::warn!(
                        entry_id = %entry_id,
                        env = %ent.client_secret_env,
                        "oauth link provider skipped (missing client secret env)"
                    );
                    continue;
                }
            };
            static_providers.insert(
                entry_id,
                OauthProviderConfig {
                    authorization_endpoint: ent.authorization_endpoint,
                    token_endpoint: ent.token_endpoint,
                    device_authorization_endpoint: ent.device_authorization_endpoint,
                    default_scopes: ent.default_scopes,
                    client_id,
                    client_secret,
                },
            );
        }
        Self {
            redirect_uri,
            static_providers,
            allowed_return_prefixes,
            runtime_providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn upsert_runtime(&self, entry_id: String, meta: RuntimeOauthProviderMeta) {
        let mut g = self.runtime_providers.write().await;
        g.insert(entry_id, meta);
    }

    pub async fn remove_runtime(&self, entry_id: &str) {
        let mut g = self.runtime_providers.write().await;
        g.remove(entry_id);
    }

    /// Replace the entire runtime map (e.g. after loading from Postgres). HTTP upserts apply until the next refresh.
    pub async fn replace_runtime_providers(
        &self,
        providers: HashMap<String, RuntimeOauthProviderMeta>,
    ) {
        let mut g = self.runtime_providers.write().await;
        *g = providers;
    }

    /// Sorted `entry_id` list for introspection (no secrets).
    pub async fn runtime_entry_ids(&self) -> Vec<String> {
        let g = self.runtime_providers.read().await;
        let mut v: Vec<String> = g.keys().cloned().collect();
        v.sort();
        v
    }

    /// Runtime wins over static catalog for the same `entry_id`. JIT-loads secret from KV for runtime entries.
    pub async fn resolve_for_oauth_start(
        &self,
        storage: &Arc<dyn SecretStore>,
        entry_id: &str,
    ) -> Result<OauthResolvedProvider, OauthResolveError> {
        let runtime_meta = {
            let rt = self.runtime_providers.read().await;
            rt.get(entry_id).cloned()
        };
        if let Some(meta) = runtime_meta {
            let key = meta.client_secret_key.as_str();
            let raw = storage
                .get_kv(key)
                .await
                .map_err(OauthResolveError::Storage)?;
            let Some(bytes) = raw else {
                return Err(OauthResolveError::SecretNotInKv);
            };
            let client_secret =
                String::from_utf8(bytes).map_err(|_| OauthResolveError::BadSecretUtf8)?;
            if client_secret.trim().is_empty() {
                return Err(OauthResolveError::SecretNotInKv);
            }
            return Ok(OauthResolvedProvider {
                authorization_endpoint: meta
                    .authorization_endpoint
                    .as_ref()
                    .map(|u| u.as_str().to_string()),
                token_endpoint: meta.token_endpoint.as_str().to_string(),
                device_authorization_endpoint: meta
                    .device_authorization_endpoint
                    .as_ref()
                    .map(|u| u.as_str().to_string()),
                default_scopes: meta.default_scopes,
                client_id: meta.client_id,
                client_secret,
            });
        }
        let Some(p) = self.static_providers.get(entry_id) else {
            return Err(OauthResolveError::UnknownEntry);
        };
        Ok(OauthResolvedProvider {
            authorization_endpoint: Some(p.authorization_endpoint.clone()),
            token_endpoint: p.token_endpoint.clone(),
            device_authorization_endpoint: p.device_authorization_endpoint.clone(),
            default_scopes: p.default_scopes.clone(),
            client_id: p.client_id.clone(),
            client_secret: p.client_secret.clone(),
        })
    }

    pub fn return_url_allowed(&self, url: &str) -> bool {
        let u = url.trim();
        if u.is_empty() {
            return false;
        }
        self.allowed_return_prefixes
            .iter()
            .any(|p| u.starts_with(p.trim()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OauthResolveError {
    #[error("OAuth provider catalog entry missing; re-link or restore provider config.")]
    UnknownEntry,
    #[error("OAuth client secret not available for refresh; check KV.")]
    SecretNotInKv,
    #[error("OAuth client secret has invalid UTF-8 encoding")]
    BadSecretUtf8,
    #[error("OAuth storage error: {0}")]
    Storage(#[source] crate::secret_store::SecretStoreError),
}

impl OauthResolveError {
    /// User-facing / log-adjacent message when OAuth refresh cannot resolve provider config.
    pub fn refresh_failure_message(&self) -> String {
        match self {
            Self::UnknownEntry => {
                "OAuth provider catalog entry missing; re-link or restore provider config.".into()
            }
            Self::SecretNotInKv | Self::BadSecretUtf8 => {
                "OAuth client secret not available for refresh; check KV.".into()
            }
            Self::Storage(s) => format!("OAuth storage error: {s}"),
        }
    }
}

fn parse_prefix_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Trim and strip a trailing `/` so `redirect_uri` matches OAuth provider allowlists.
fn normalize_plasm_oauth_redirect_uri(raw: impl Into<String>) -> String {
    let s = raw.into();
    let t = s.trim();
    t.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn default_return_url_allowlist_local_dev() {
        let c = OauthLinkCatalog::default();
        assert!(c.return_url_allowed("http://127.0.0.1:4000/w/t/p/outbound-auth/return"));
        assert!(c.return_url_allowed("http://localhost:4000/cb"));
        assert!(!c.return_url_allowed("https://evil.example/phish"));
        assert!(!c.return_url_allowed(""));
    }

    #[tokio::test]
    async fn replace_runtime_providers_sets_runtime_map() {
        let c = OauthLinkCatalog::default();
        assert!(c.runtime_entry_ids().await.is_empty());
        let mut m = HashMap::new();
        m.insert(
            "acme".into(),
            RuntimeOauthProviderMeta::try_new(
                "https://a.example/authorize",
                "https://a.example/token",
                vec![],
                "cid",
                "plasm:oauth_app:v1:test",
            )
            .expect("valid test meta"),
        );
        c.replace_runtime_providers(m).await;
        assert_eq!(c.runtime_entry_ids().await, vec!["acme".to_string()]);
    }
}
