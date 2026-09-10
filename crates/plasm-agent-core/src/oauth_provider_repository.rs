//! sqlx persistence for `oauth_provider_apps` (outbound OAuth provider registry).

use crate::traced_pg::PgPool;
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize)]
pub struct OauthProviderAppRow {
    pub entry_id: String,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub device_authorization_endpoint: Option<String>,
    pub client_id: String,
    pub client_secret_key: String,
    pub enabled: bool,
}

pub async fn list_oauth_provider_apps(
    pool: &PgPool,
) -> Result<Vec<OauthProviderAppRow>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT entry_id,
               authorization_endpoint,
               token_endpoint,
               device_authorization_endpoint,
               client_id,
               client_secret_key,
               enabled
        FROM public.oauth_provider_apps
        ORDER BY entry_id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(OauthProviderAppRow {
            entry_id: row.try_get("entry_id")?,
            authorization_endpoint: row.try_get("authorization_endpoint")?,
            token_endpoint: row.try_get("token_endpoint")?,
            device_authorization_endpoint: row.try_get("device_authorization_endpoint")?,
            client_id: row.try_get("client_id")?,
            client_secret_key: row.try_get("client_secret_key")?,
            enabled: row.try_get("enabled")?,
        });
    }
    Ok(out)
}

pub struct UpsertOauthProviderParams<'a> {
    pub entry_id: &'a str,
    pub authorization_endpoint: Option<&'a str>,
    pub token_endpoint: &'a str,
    pub device_authorization_endpoint: Option<&'a str>,
    pub client_id: &'a str,
    pub client_secret_key: &'a str,
    pub enabled: bool,
}

pub async fn upsert_oauth_provider_app(
    pool: &PgPool,
    p: UpsertOauthProviderParams<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO oauth_provider_apps (
            entry_id,
            authorization_endpoint,
            token_endpoint,
            device_authorization_endpoint,
            client_id,
            client_secret_key,
            enabled,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, now())
        ON CONFLICT (entry_id)
        DO UPDATE SET
            authorization_endpoint = EXCLUDED.authorization_endpoint,
            token_endpoint = EXCLUDED.token_endpoint,
            device_authorization_endpoint = EXCLUDED.device_authorization_endpoint,
            client_id = EXCLUDED.client_id,
            client_secret_key = EXCLUDED.client_secret_key,
            enabled = EXCLUDED.enabled,
            updated_at = now()
        "#,
    )
    .bind(p.entry_id)
    .bind(empty_as_none(p.authorization_endpoint))
    .bind(p.token_endpoint)
    .bind(empty_as_none(p.device_authorization_endpoint))
    .bind(p.client_id)
    .bind(p.client_secret_key)
    .bind(p.enabled)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_oauth_provider_enabled(
    pool: &PgPool,
    entry_id: &str,
    enabled: bool,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        r#"
        UPDATE oauth_provider_apps
        SET enabled = $2, updated_at = now()
        WHERE entry_id = $1
        "#,
    )
    .bind(entry_id)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

fn empty_as_none(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|t| !t.is_empty())
}
