//! Pluggable source of runtime OAuth provider rows (I/O boundary). Postgres implementation reads
//! `public.oauth_provider_apps`; tests may use [`StubOauthRuntimeProviderSource`].

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::traced_pg::PgPool;
use sqlx::Row;
use thiserror::Error;

use crate::oauth_link_catalog::OauthLinkCatalog;
use crate::oauth_provider_model::RuntimeOauthProviderMeta;

#[derive(Debug, Error)]
pub enum OauthRuntimeFetchError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// Boxed future returned by [`OauthRuntimeProviderSource::fetch_runtime_map`].
pub type OauthRuntimeMapFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<HashMap<String, RuntimeOauthProviderMeta>, OauthRuntimeFetchError>,
            > + Send
            + 'a,
    >,
>;

/// Async snapshot of runtime provider metadata (replaces the catalog runtime map when applied).
pub trait OauthRuntimeProviderSource: Send + Sync {
    fn fetch_runtime_map(&self) -> OauthRuntimeMapFuture<'_>;
}

/// Reads enabled rows from `public.oauth_provider_apps`.
#[derive(Clone)]
pub struct PostgresOauthRuntimeProviderSource {
    pool: PgPool,
}

impl PostgresOauthRuntimeProviderSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl OauthRuntimeProviderSource for PostgresOauthRuntimeProviderSource {
    fn fetch_runtime_map(&self) -> OauthRuntimeMapFuture<'_> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let rows = sqlx::query(
                r#"
                SELECT entry_id,
                       authorization_endpoint,
                       token_endpoint,
                       device_authorization_endpoint,
                       client_id,
                       client_secret_key
                FROM public.oauth_provider_apps
                WHERE enabled = true
                "#,
            )
            .fetch_all(&pool)
            .await
            .map_err(OauthRuntimeFetchError::from)?;

            let mut map = HashMap::with_capacity(rows.len());
            for row in rows {
                let entry_id: String = row
                    .try_get("entry_id")
                    .map_err(OauthRuntimeFetchError::from)?;
                let auth_ep: Option<String> = row
                    .try_get("authorization_endpoint")
                    .map_err(OauthRuntimeFetchError::from)?;
                let token_ep: Option<String> = row
                    .try_get("token_endpoint")
                    .map_err(OauthRuntimeFetchError::from)?;
                let device_ep: Option<String> = row
                    .try_get("device_authorization_endpoint")
                    .map_err(OauthRuntimeFetchError::from)?;
                let client_id: String = row
                    .try_get("client_id")
                    .map_err(OauthRuntimeFetchError::from)?;
                let client_secret_key: String = row
                    .try_get("client_secret_key")
                    .map_err(OauthRuntimeFetchError::from)?;

                let entry_id = entry_id.trim();
                let auth_ep_raw = auth_ep.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let token_ep = token_ep.as_deref().unwrap_or("").trim();
                let device_ep_raw = device_ep
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let client_id_s = client_id.trim();
                let client_secret_key = client_secret_key.trim();

                match RuntimeOauthProviderMeta::try_from_parts(
                    auth_ep_raw,
                    token_ep,
                    device_ep_raw,
                    Vec::new(),
                    client_id_s,
                    client_secret_key,
                ) {
                    Ok(meta) => {
                        map.insert(entry_id.to_string(), meta);
                    }
                    Err(e) => {
                        tracing::warn!(
                            entry_id = %entry_id,
                            error = %e,
                            "oauth_runtime_source: skip invalid oauth_provider_apps row"
                        );
                    }
                }
            }

            Ok(map)
        })
    }
}

/// Apply a fetched snapshot to the catalog (full replace).
pub async fn apply_runtime_source_to_catalog(
    source: &dyn OauthRuntimeProviderSource,
    catalog: &OauthLinkCatalog,
) -> Result<usize, OauthRuntimeFetchError> {
    let map = source.fetch_runtime_map().await?;
    let n = map.len();
    catalog.replace_runtime_providers(map).await;
    Ok(n)
}

#[cfg(test)]
pub struct StubOauthRuntimeProviderSource {
    pub map: HashMap<String, RuntimeOauthProviderMeta>,
}

#[cfg(test)]
impl OauthRuntimeProviderSource for StubOauthRuntimeProviderSource {
    fn fetch_runtime_map(&self) -> OauthRuntimeMapFuture<'_> {
        let m = self.map.clone();
        Box::pin(async move { Ok(m) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_source_replaces_catalog_runtime_map() {
        let catalog = OauthLinkCatalog::default();
        let meta = RuntimeOauthProviderMeta::try_new(
            "https://a.example/oauth/authorize",
            "https://a.example/oauth/token",
            vec![],
            "cid",
            "plasm:outbound:test:key",
        )
        .expect("meta");
        let mut map = HashMap::new();
        map.insert("stub_entry".into(), meta);
        let stub = StubOauthRuntimeProviderSource { map };
        let n = apply_runtime_source_to_catalog(&stub, &catalog)
            .await
            .expect("apply");
        assert_eq!(n, 1);
        assert_eq!(
            catalog.runtime_entry_ids().await,
            vec!["stub_entry".to_string()]
        );
    }
}
