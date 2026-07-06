//! Readiness for active MCP upsert when `auth_config_by_entry` is pending (no binding row yet).
//!
//! Mirrors personal Connect apps → API key → allowlist expand (Vultr-style catalogs).

mod support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auth_framework::storage::{AuthStorage, MemoryStorage};
use plasm_agent_core::binding_store::entry_secret_present_for_upsert;
use plasm_agent_core::mcp_config_readiness::catalog_entry_readiness_gaps;
use plasm_agent_core::mcp_config_repository::McpConfigRepository;
use plasm_agent_core::mcp_runtime_config::McpRuntimeConfig;
use sqlx::PgPool;
use support::postgres::{integration_postgres_url, INTEGRATION_POSTGRES_URL_ENV};
use uuid::Uuid;

const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

async fn ensure_outbound_tables(pool: &PgPool) {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS project_outbound_auth_configs (
            id UUID PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            workspace_slug TEXT NOT NULL,
            project_slug TEXT NOT NULL,
            space_type TEXT NOT NULL DEFAULT 'organization',
            owner_subject TEXT,
            registry_entry_id TEXT NOT NULL,
            auth_kind TEXT NOT NULL,
            name TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'enabled',
            oauth_scope_set_name TEXT,
            oauth_scopes TEXT[] NOT NULL DEFAULT '{}',
            inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE TABLE IF NOT EXISTS project_outbound_connected_accounts (
            id UUID PRIMARY KEY,
            auth_config_id UUID NOT NULL REFERENCES project_outbound_auth_configs (id) ON DELETE CASCADE,
            owner_subject TEXT,
            external_user_id TEXT,
            hosted_kv_key TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'active',
            granted_scopes TEXT[] NOT NULL DEFAULT '{}',
            last_connected_at TIMESTAMPTZ,
            last_oauth_error TEXT,
            last_oauth_error_at TIMESTAMPTZ,
            inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE UNIQUE INDEX IF NOT EXISTS project_outbound_connected_accounts_hosted_kv_key
            ON project_outbound_connected_accounts (hosted_kv_key);
        "#,
    )
    .execute(pool)
    .await
    .expect("outbound DDL");
}

fn runtime_cfg(config_id: Uuid, auth_config_id: Uuid, include_notion: bool) -> McpRuntimeConfig {
    let mut allowed = HashSet::new();
    if include_notion {
        allowed.insert("notion".into());
    }
    let mut auth_config_by_entry = HashMap::new();
    if include_notion {
        auth_config_by_entry.insert("notion".into(), auth_config_id);
    }
    McpRuntimeConfig {
        id: config_id,
        tenant_id: "tenant-readiness".into(),
        space_type: "personal".into(),
        owner_subject: Some("github:readiness-test".into()),
        version: 2,
        endpoint_secret_hash: [0u8; 32],
        credential_secret_hashes: HashSet::new(),
        allowed_entry_ids: allowed,
        capabilities_by_entry: HashMap::new(),
        auth_config_by_entry,
    }
}

async fn seed_connected_account(
    pool: &PgPool,
    auth_config_id: Uuid,
    kv_key: &str,
    owner_subject: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO project_outbound_auth_configs
            (id, tenant_id, workspace_slug, project_slug, space_type, owner_subject,
             registry_entry_id, auth_kind, name, status)
        VALUES ($1, 'tenant-readiness', 'ws', 'main', 'personal', $2, 'notion', 'api_key', 'Notion', 'enabled')
        "#,
    )
    .bind(auth_config_id)
    .bind(owner_subject)
    .execute(pool)
    .await
    .expect("auth config");

    sqlx::query(
        r#"
        INSERT INTO project_outbound_connected_accounts
            (id, auth_config_id, owner_subject, hosted_kv_key, status, last_connected_at)
        VALUES ($1, $2, $3, $4, 'active', now())
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(auth_config_id)
    .bind(owner_subject)
    .bind(kv_key)
    .execute(pool)
    .await
    .expect("connected account");
}

#[tokio::test]
async fn pending_auth_config_secret_readiness_succeeds_without_binding_row() {
    let Some((_, db_url)) = integration_postgres_url(START_TIMEOUT).await else {
        eprintln!(
            "mcp_readiness_pending_auth: skipping (no Docker / Postgres). \
             Set {INTEGRATION_POSTGRES_URL_ENV} or ensure Docker is running."
        );
        return;
    };

    let repo = McpConfigRepository::connect_and_migrate(&db_url)
        .await
        .expect("migrate");
    ensure_outbound_tables(repo.pool()).await;

    let config_id = Uuid::new_v4();
    let auth_config_id = Uuid::new_v4();
    let kv_key = format!("plasm:outbound:v1:{}", Uuid::new_v4());
    let owner = "github:readiness-test";

    seed_connected_account(repo.pool(), auth_config_id, &kv_key, owner).await;

    let storage: Arc<dyn AuthStorage> = Arc::new(MemoryStorage::new());
    storage
        .store_kv(&kv_key, b"test-api-key-secret", None)
        .await
        .expect("store kv");

    let cfg = runtime_cfg(config_id, auth_config_id, true);
    assert!(
        entry_secret_present_for_upsert(&repo, Some(&storage), &cfg, "notion").await,
        "expected secret via pending auth_config_id before binding row exists"
    );

    let optional = HashSet::new();
    let gaps =
        catalog_entry_readiness_gaps(&repo, Some(&storage), &cfg, "notion", &optional, true).await;
    assert!(
        gaps.is_empty(),
        "expected no readiness gaps when connected account + KV exist: {gaps:?}"
    );
}

#[tokio::test]
async fn pending_auth_config_secret_readiness_fails_without_connected_account() {
    let Some((_, db_url)) = integration_postgres_url(START_TIMEOUT).await else {
        eprintln!(
            "mcp_readiness_pending_auth: skipping (no Docker / Postgres). \
             Set {INTEGRATION_POSTGRES_URL_ENV} or ensure Docker is running."
        );
        return;
    };

    let repo = McpConfigRepository::connect_and_migrate(&db_url)
        .await
        .expect("migrate");
    ensure_outbound_tables(repo.pool()).await;

    let config_id = Uuid::new_v4();
    let auth_config_id = Uuid::new_v4();
    let storage: Arc<dyn AuthStorage> = Arc::new(MemoryStorage::new());
    let cfg = runtime_cfg(config_id, auth_config_id, true);

    assert!(
        !entry_secret_present_for_upsert(&repo, Some(&storage), &cfg, "notion").await,
        "expected no secret when connected account is absent"
    );

    let optional = HashSet::new();
    let gaps =
        catalog_entry_readiness_gaps(&repo, Some(&storage), &cfg, "notion", &optional, true).await;
    assert!(
        gaps.iter()
            .any(|g| g.gap == plasm_agent_core::binding_store::ReadinessGapKind::Secret),
        "expected Secret gap without connected account: {gaps:?}"
    );
}

#[tokio::test]
async fn graph_binding_join_still_resolves_secret_when_binding_row_exists() {
    let Some((_, db_url)) = integration_postgres_url(START_TIMEOUT).await else {
        eprintln!(
            "mcp_readiness_pending_auth: skipping (no Docker / Postgres). \
             Set {INTEGRATION_POSTGRES_URL_ENV} or ensure Docker is running."
        );
        return;
    };

    let repo = McpConfigRepository::connect_and_migrate(&db_url)
        .await
        .expect("migrate");
    ensure_outbound_tables(repo.pool()).await;

    let config_id = Uuid::new_v4();
    let auth_config_id = Uuid::new_v4();
    let kv_key = format!("plasm:outbound:v1:{}", Uuid::new_v4());
    let owner = "github:readiness-test";

    seed_connected_account(repo.pool(), auth_config_id, &kv_key, owner).await;

    let runtime = runtime_cfg(config_id, auth_config_id, true);
    repo.upsert_full(runtime, "ws", "main", "Your MCP", "active", &[])
        .await
        .expect("seed config with binding row");

    let storage: Arc<dyn AuthStorage> = Arc::new(MemoryStorage::new());
    storage
        .store_kv(&kv_key, b"bound-secret", None)
        .await
        .expect("store kv");

    let cfg = runtime_cfg(config_id, auth_config_id, true);
    assert!(
        entry_secret_present_for_upsert(&repo, Some(&storage), &cfg, "notion").await,
        "expected secret via persisted graph binding join"
    );
}
