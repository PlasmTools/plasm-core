//! Load outbound OAuth link **runtime** provider metadata from Postgres (`oauth_provider_apps`),
//! same rows Phoenix Ops writes. Replaces the in-memory runtime map on each refresh so agent
//! restarts stay aligned with the database. Client secrets remain in auth-framework KV at
//! `client_secret_key` (unchanged).

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tokio::time::sleep;

use crate::oauth_link_catalog::OauthLinkCatalog;
use crate::oauth_runtime_source::{
    apply_runtime_source_to_catalog, OauthRuntimeFetchError, PostgresOauthRuntimeProviderSource,
};

/// Database URL for reading `public.oauth_provider_apps`. Falls back so local dev matches Phoenix.
pub fn outbound_oauth_provider_database_url() -> Option<String> {
    for key in [
        "PLASM_OAUTH_PROVIDER_DATABASE_URL",
        "PLASM_AUTH_STORAGE_URL",
        "DATABASE_URL",
    ] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn refresh_interval_from_env() -> Duration {
    let secs: u64 = std::env::var("PLASM_OAUTH_PROVIDER_REFRESH_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs)
}

/// Parsed env for the Postgres pull loop (composition root reads env; this struct is explicit input).
#[derive(Clone, Debug)]
pub struct OauthProviderPullSettings {
    pub database_url: String,
    pub refresh_interval: Duration,
}

impl OauthProviderPullSettings {
    pub fn from_env() -> Option<Self> {
        let database_url = outbound_oauth_provider_database_url()?;
        Some(Self {
            database_url,
            refresh_interval: refresh_interval_from_env(),
        })
    }
}

/// Result of attempting to connect and run the initial refresh (for logging / ops).
#[derive(Debug)]
pub enum OauthProviderPullInitOutcome {
    ConnectFailed {
        error: String,
    },
    Ran {
        /// First snapshot apply after connect (logs duplicate this; kept for future callers / tests).
        #[allow(dead_code)]
        initial_refresh: Result<usize, OauthRuntimeFetchError>,
        periodic_spawned: bool,
    },
}

/// Connect, await one refresh (call before accepting traffic), then optionally spawn periodic refresh.
pub async fn init_oauth_provider_pull_from_postgres(
    catalog: Arc<OauthLinkCatalog>,
    settings: OauthProviderPullSettings,
) -> OauthProviderPullInitOutcome {
    let pool = match PgPoolOptions::new()
        .max_connections(2)
        .connect(&settings.database_url)
        .await
    {
        Ok(p) => Arc::new(crate::traced_pg::wrap(p)),
        Err(e) => {
            return OauthProviderPullInitOutcome::ConnectFailed {
                error: e.to_string(),
            };
        }
    };

    let source = PostgresOauthRuntimeProviderSource::new((*pool).clone());
    let initial_refresh = apply_runtime_source_to_catalog(&source, catalog.as_ref()).await;

    match &initial_refresh {
        Ok(n) => tracing::info!(
            count = n,
            "oauth_provider_pull: initial refresh from Postgres"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "oauth_provider_pull: initial refresh failed (OAuth may 404 until DB is reachable)"
        ),
    }

    let interval = settings.refresh_interval;
    if interval.is_zero() {
        return OauthProviderPullInitOutcome::Ran {
            initial_refresh,
            periodic_spawned: false,
        };
    }

    let cat = Arc::clone(&catalog);
    let pool_bg = Arc::clone(&pool);
    tokio::spawn(async move {
        let src = PostgresOauthRuntimeProviderSource::new((*pool_bg).clone());
        loop {
            sleep(interval).await;
            match apply_runtime_source_to_catalog(&src, cat.as_ref()).await {
                Ok(n) => tracing::debug!(count = n, "oauth_provider_pull: periodic refresh"),
                Err(e) => {
                    tracing::warn!(error = %e, "oauth_provider_pull: periodic refresh failed")
                }
            }
        }
    });

    OauthProviderPullInitOutcome::Ran {
        initial_refresh,
        periodic_spawned: true,
    }
}
