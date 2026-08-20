//! sqlx-backed persistence for tenant MCP configuration (`project_mcp_*` tables).

use std::collections::{HashMap, HashSet};

use crate::traced_pg::PgPool;
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Postgres, QueryBuilder, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::mcp_runtime_config::McpRuntimeConfig;

#[derive(Debug, Error)]
pub enum McpConfigRepositoryError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(
        "project_mcp_configs is missing after sqlx migrate; \
         check DATABASE_URL / PLASM_MCP_CONFIG_DATABASE_URL and Postgres permissions"
    )]
    PostMigrateSchemaMissing,
    #[error("{0}")]
    InvalidInput(String),
}

/// For [`McpConfigRepository::fetch_hosted_kv_for_graph_binding`]: `None` and blank/whitespace-only
/// subjects select the most recent active account regardless of `owner_subject` (SQL `NULL`/empty bind).
pub fn normalize_graph_binding_owner_subject(subject: Option<&str>) -> Option<&str> {
    subject.map(str::trim).filter(|s| !s.is_empty())
}

/// Resolves the SQL `owner_subject` bind for [`McpConfigRepository::fetch_hosted_kv_for_graph_binding`].
///
/// MCP API key transport uses [`crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::verify_api_key`]
/// which sets the principal subject to the config UUID when `owner_subject` is unset on the config
/// row. That synthetic value must **not** filter `project_outbound_connected_accounts` (real rows use
/// GitHub subject, etc.); pass `None` so the query picks the most recently connected active account.
pub fn effective_owner_subject_for_hosted_kv<'a>(
    config_id: Uuid,
    cfg_owner_subject: Option<&'a str>,
    principal_subject: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(s) = normalize_graph_binding_owner_subject(cfg_owner_subject) {
        return Some(s);
    }
    let config_id_str = config_id.to_string();
    match principal_subject.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s == config_id_str.as_str() => None,
        Some(s) => normalize_graph_binding_owner_subject(Some(s)),
        None => None,
    }
}

/// Resolve the Postgres URL used for MCP configuration rows.
pub fn mcp_config_database_url() -> Option<String> {
    std::env::var("PLASM_MCP_CONFIG_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("PLASM_AUTH_STORAGE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("DATABASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

#[derive(Clone)]
pub struct McpConfigRepository {
    pool: PgPool,
}

async fn project_mcp_configs_table_exists(pool: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
            SELECT 1
            FROM pg_catalog.pg_class c
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = 'public'
              AND c.relname = 'project_mcp_configs'
              AND c.relkind = 'r'
        )"#,
    )
    .fetch_one(pool)
    .await
}

async fn sqlx_migrations_table_exists(pool: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
            SELECT 1
            FROM pg_catalog.pg_class c
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = 'public'
              AND c.relname = '_sqlx_migrations'
              AND c.relkind = 'r'
        )"#,
    )
    .fetch_one(pool)
    .await
}

/// Pre-squash migration versions (see `migrations/20260601000000_plasm_agent_schema.sql`).
const LEGACY_SQUASHED_MIGRATION_VERSIONS: &[i64] = &[
    20260216120000,
    20260510140000,
    20260511120000,
    20260512130000,
];

async fn delete_migration_ledger_version(
    pool: &sqlx::PgPool,
    version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = $1")
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}

/// Remove ledger rows for migrations squashed out of the embedded set so `sqlx::migrate` can run.
async fn prune_squashed_migration_ledger(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    if !sqlx_migrations_table_exists(pool).await? {
        return Ok(());
    }
    for version in LEGACY_SQUASHED_MIGRATION_VERSIONS {
        delete_migration_ledger_version(pool, *version).await?;
    }
    Ok(())
}

async fn run_mcp_config_migrations(pool: &sqlx::PgPool) -> Result<(), McpConfigRepositoryError> {
    prune_squashed_migration_ledger(pool).await?;
    let migrator = sqlx::migrate!("./migrations");
    match migrator.run(pool).await {
        Ok(()) => Ok(()),
        Err(sqlx::migrate::MigrateError::VersionMissing(version)) => {
            delete_migration_ledger_version(pool, version).await?;
            migrator.run(pool).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

impl McpConfigRepository {
    pub async fn connect_and_migrate(database_url: &str) -> Result<Self, McpConfigRepositoryError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        if !project_mcp_configs_table_exists(&pool).await? {
            prune_squashed_migration_ledger(&pool).await?;
        }
        run_mcp_config_migrations(&pool).await?;
        if !project_mcp_configs_table_exists(&pool).await? {
            return Err(McpConfigRepositoryError::PostMigrateSchemaMissing);
        }
        let pool = crate::traced_pg::wrap(pool);
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn has_tenant_configs(&self) -> Result<bool, sqlx::Error> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM project_mcp_configs WHERE status = 'active'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(n > 0)
    }

    pub async fn get_runtime_config(
        &self,
        id: &Uuid,
    ) -> Result<Option<McpRuntimeConfig>, sqlx::Error> {
        self.load_runtime_for_config(*id).await
    }

    /// Resolve the active Plasm-hosted outbound secret key for a catalog `entry_id` bound to this
    /// MCP config (Phoenix `project_mcp_auth_bindings` → `project_outbound_connected_accounts`).
    ///
    /// When `owner_subject` is `Some`, rows are restricted to that GitHub (or other) subject; when
    /// `None`, the most recently connected active account for the binding wins.
    pub async fn fetch_hosted_kv_for_graph_binding(
        &self,
        config_id: Uuid,
        entry_id: &str,
        owner_subject: Option<&str>,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<String> = sqlx::query_scalar(
            r#"
            SELECT ca.hosted_kv_key
            FROM project_mcp_auth_bindings b
            INNER JOIN project_outbound_connected_accounts ca
              ON ca.auth_config_id = b.auth_config_id
            WHERE b.config_id = $1
              AND b.entry_id = $2
              AND ca.status = 'active'
              AND (
                $3::text IS NULL
                OR TRIM($3::text) = ''
                OR ca.owner_subject = $3
              )
            ORDER BY ca.last_connected_at DESC NULLS LAST
            LIMIT 1
            "#,
        )
        .bind(config_id)
        .bind(entry_id)
        .bind(owner_subject)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Resolve the active Plasm-hosted outbound secret key for an outbound auth config row
    /// (`project_outbound_connected_accounts`), without requiring a persisted
    /// `project_mcp_auth_bindings` row.
    ///
    /// Used during **active** MCP config upsert readiness when Phoenix sends a pending
    /// `auth_config_by_entry` before the binding row is written.
    pub async fn fetch_hosted_kv_for_auth_config(
        &self,
        auth_config_id: Uuid,
        owner_subject: Option<&str>,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<String> = sqlx::query_scalar(
            r#"
            SELECT hosted_kv_key
            FROM project_outbound_connected_accounts
            WHERE auth_config_id = $1
              AND status = 'active'
              AND (
                $2::text IS NULL
                OR TRIM($2::text) = ''
                OR owner_subject = $2
              )
            ORDER BY last_connected_at DESC NULLS LAST
            LIMIT 1
            "#,
        )
        .bind(auth_config_id)
        .bind(owner_subject)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Binding KV pointer for `(config_id, entry_id)` scoped triple.
    pub async fn fetch_binding_kv_key_for_scope(
        &self,
        config_id: Uuid,
        entry_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<String> = sqlx::query_scalar(
            r#"
            SELECT binding_kv_key
            FROM project_mcp_entry_bindings
            WHERE config_id = $1 AND entry_id = $2
            "#,
        )
        .bind(config_id)
        .bind(entry_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn upsert_entry_binding(
        &self,
        scope: crate::binding_slots::BindingScope,
        binding_kv_key: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO project_mcp_entry_bindings
                (id, tenant_id, config_id, entry_id, binding_kv_key, inserted_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (config_id, entry_id) DO UPDATE SET
                tenant_id = EXCLUDED.tenant_id,
                binding_kv_key = EXCLUDED.binding_kv_key,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&scope.tenant_id)
        .bind(scope.mcp_config_id)
        .bind(&scope.entry_id)
        .bind(binding_kv_key)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load runtime config for active or disabled MCP bundles (binding connect).
    pub async fn load_runtime_for_config_including_disabled(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpRuntimeConfig>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, tenant_id, workspace_slug, project_slug, space_type, owner_subject, config_version, endpoint_secret_hash
            FROM project_mcp_configs
            WHERE id = $1 AND status IN ('active', 'disabled')
            "#,
        )
        .bind(config_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let id: Uuid = row.get("id");
        let tenant_id: String = row.get("tenant_id");
        let workspace_slug: String = row.get("workspace_slug");
        let project_slug: String = row.get("project_slug");
        let space_type: String = row.get("space_type");
        let owner_subject: Option<String> = row.get("owner_subject");
        let version: i64 = row.get("config_version");
        let endpoint_secret_hash: Vec<u8> = row.get("endpoint_secret_hash");
        if endpoint_secret_hash.len() != 32 {
            return Ok(None);
        }
        let mut endpoint_arr = [0u8; 32];
        endpoint_arr.copy_from_slice(&endpoint_secret_hash);

        let graph_rows = sqlx::query(
            "SELECT entry_id, enabled FROM project_mcp_allowed_graphs WHERE config_id = $1",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let graphs: Vec<(String, bool)> = graph_rows
            .into_iter()
            .map(|r| (r.get::<String, _>("entry_id"), r.get::<bool, _>("enabled")))
            .collect();

        let mut allowed_entry_ids = HashSet::new();
        for (eid, en) in graphs {
            if en && !eid.is_empty() {
                allowed_entry_ids.insert(eid);
            }
        }

        let cap_rows = sqlx::query(
            r#"SELECT entry_id, capability_name FROM project_mcp_allowed_capabilities
               WHERE config_id = $1 AND enabled = true"#,
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let caps: Vec<(String, String)> = cap_rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("entry_id"),
                    r.get::<String, _>("capability_name"),
                )
            })
            .collect();

        let mut capabilities_by_entry: HashMap<String, HashSet<String>> = HashMap::new();
        for (entry_id, cap_name) in caps {
            if entry_id.is_empty() || cap_name.is_empty() {
                continue;
            }
            capabilities_by_entry
                .entry(entry_id)
                .or_default()
                .insert(cap_name);
        }

        for eid in &allowed_entry_ids {
            if !capabilities_by_entry.contains_key(eid) {
                capabilities_by_entry.insert(eid.clone(), HashSet::new());
            }
        }

        let bind_rows = sqlx::query(
            "SELECT entry_id, auth_config_id FROM project_mcp_auth_bindings WHERE config_id = $1",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let bindings: Vec<(String, Uuid)> = bind_rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("entry_id"),
                    r.get::<Uuid, _>("auth_config_id"),
                )
            })
            .collect();

        let mut auth_config_by_entry = HashMap::new();
        for (eid, aid) in bindings {
            if !eid.is_empty() {
                auth_config_by_entry.insert(eid, aid);
            }
        }

        let cred_rows = sqlx::query(
            "SELECT secret_hash FROM project_mcp_credentials WHERE config_id = $1 AND status = 'active'",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let cred_hashes: Vec<Vec<u8>> = cred_rows
            .into_iter()
            .map(|r| r.get::<Vec<u8>, _>("secret_hash"))
            .collect();

        let mut credential_secret_hashes = HashSet::new();
        for bytes in cred_hashes {
            if bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                credential_secret_hashes.insert(arr);
            }
        }

        Ok(Some(McpRuntimeConfig {
            id,
            tenant_id,
            workspace_slug,
            project_slug,
            space_type,
            owner_subject,
            version: version as u64,
            endpoint_secret_hash: endpoint_arr,
            credential_secret_hashes,
            allowed_entry_ids,
            capabilities_by_entry,
            auth_config_by_entry,
        }))
    }

    pub async fn find_personal_runtime(
        &self,
        tenant_id: &str,
        owner_subject: &str,
    ) -> Result<Option<McpRuntimeConfig>, sqlx::Error> {
        let id: Option<Uuid> = sqlx::query_scalar(
            r#"
            SELECT id FROM project_mcp_configs
            WHERE tenant_id = $1 AND space_type = 'personal' AND owner_subject = $2 AND status = 'active'
            LIMIT 1
            "#,
        )
        .bind(tenant_id)
        .bind(owner_subject)
        .fetch_optional(&self.pool)
        .await?;

        let Some(id) = id else {
            return Ok(None);
        };
        self.load_runtime_for_config(id).await
    }

    async fn load_runtime_for_config(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpRuntimeConfig>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, tenant_id, workspace_slug, project_slug, space_type, owner_subject, config_version, endpoint_secret_hash
            FROM project_mcp_configs
            WHERE id = $1 AND status = 'active'
            "#,
        )
        .bind(config_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let id: Uuid = row.get("id");
        let tenant_id: String = row.get("tenant_id");
        let workspace_slug: String = row.get("workspace_slug");
        let project_slug: String = row.get("project_slug");
        let space_type: String = row.get("space_type");
        let owner_subject: Option<String> = row.get("owner_subject");
        let version: i64 = row.get("config_version");
        let endpoint_secret_hash: Vec<u8> = row.get("endpoint_secret_hash");
        if endpoint_secret_hash.len() != 32 {
            return Ok(None);
        }
        let mut endpoint_arr = [0u8; 32];
        endpoint_arr.copy_from_slice(&endpoint_secret_hash);

        let graph_rows = sqlx::query(
            "SELECT entry_id, enabled FROM project_mcp_allowed_graphs WHERE config_id = $1",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let graphs: Vec<(String, bool)> = graph_rows
            .into_iter()
            .map(|r| (r.get::<String, _>("entry_id"), r.get::<bool, _>("enabled")))
            .collect();

        let mut allowed_entry_ids = HashSet::new();
        for (eid, en) in graphs {
            if en && !eid.is_empty() {
                allowed_entry_ids.insert(eid);
            }
        }

        let cap_rows = sqlx::query(
            r#"SELECT entry_id, capability_name FROM project_mcp_allowed_capabilities
               WHERE config_id = $1 AND enabled = true"#,
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let caps: Vec<(String, String)> = cap_rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("entry_id"),
                    r.get::<String, _>("capability_name"),
                )
            })
            .collect();

        let mut capabilities_by_entry: HashMap<String, HashSet<String>> = HashMap::new();
        for (entry_id, cap_name) in caps {
            if entry_id.is_empty() || cap_name.is_empty() {
                continue;
            }
            capabilities_by_entry
                .entry(entry_id)
                .or_default()
                .insert(cap_name);
        }

        for eid in &allowed_entry_ids {
            if !capabilities_by_entry.contains_key(eid) {
                capabilities_by_entry.insert(eid.clone(), HashSet::new());
            }
        }

        let bind_rows = sqlx::query(
            "SELECT entry_id, auth_config_id FROM project_mcp_auth_bindings WHERE config_id = $1",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let bindings: Vec<(String, Uuid)> = bind_rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("entry_id"),
                    r.get::<Uuid, _>("auth_config_id"),
                )
            })
            .collect();

        let mut auth_config_by_entry = HashMap::new();
        for (eid, aid) in bindings {
            if !eid.is_empty() {
                auth_config_by_entry.insert(eid, aid);
            }
        }

        let cred_rows = sqlx::query(
            "SELECT secret_hash FROM project_mcp_credentials WHERE config_id = $1 AND status = 'active'",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let cred_hashes: Vec<Vec<u8>> = cred_rows
            .into_iter()
            .map(|r| r.get::<Vec<u8>, _>("secret_hash"))
            .collect();

        let mut credential_secret_hashes = HashSet::new();
        for bytes in cred_hashes {
            if bytes.len() == 32 {
                let mut h = [0u8; 32];
                h.copy_from_slice(&bytes);
                credential_secret_hashes.insert(h);
            }
        }

        Ok(Some(McpRuntimeConfig {
            id,
            tenant_id,
            workspace_slug,
            project_slug,
            space_type,
            owner_subject,
            version: version as u64,
            endpoint_secret_hash: endpoint_arr,
            credential_secret_hashes,
            allowed_entry_ids,
            capabilities_by_entry,
            auth_config_by_entry,
        }))
    }

    pub async fn upsert_full(
        &self,
        runtime: McpRuntimeConfig,
        workspace_slug: &str,
        project_slug: &str,
        name: &str,
        status: &str,
        auth_optional_entry_ids: &[String],
    ) -> Result<(), McpConfigRepositoryError> {
        let mut tx = self.pool.begin().await?;

        let ws = workspace_slug.trim();
        let ps = project_slug.trim();
        let nm = name.trim();
        let st = status.trim();
        if ws.is_empty() || ps.is_empty() || nm.is_empty() {
            return Err(McpConfigRepositoryError::InvalidInput(
                "workspace_slug, project_slug, and name must be non-empty".into(),
            ));
        }

        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO project_mcp_configs (
                id, tenant_id, workspace_slug, project_slug, space_type, owner_subject,
                name, status, config_version, endpoint_secret_hash, mcp_api_key_fingerprint,
                auth_optional_entry_ids, inserted_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, $11, $12, $13)
            ON CONFLICT (id) DO UPDATE SET
                tenant_id = EXCLUDED.tenant_id,
                workspace_slug = EXCLUDED.workspace_slug,
                project_slug = EXCLUDED.project_slug,
                space_type = EXCLUDED.space_type,
                owner_subject = EXCLUDED.owner_subject,
                name = EXCLUDED.name,
                status = EXCLUDED.status,
                config_version = EXCLUDED.config_version,
                endpoint_secret_hash = EXCLUDED.endpoint_secret_hash,
                auth_optional_entry_ids = EXCLUDED.auth_optional_entry_ids,
                mcp_api_key_fingerprint = project_mcp_configs.mcp_api_key_fingerprint,
                inserted_at = project_mcp_configs.inserted_at,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(runtime.id)
        .bind(&runtime.tenant_id)
        .bind(ws)
        .bind(ps)
        .bind(&runtime.space_type)
        .bind(&runtime.owner_subject)
        .bind(nm)
        .bind(st)
        .bind(runtime.version as i64)
        .bind(&runtime.endpoint_secret_hash[..])
        .bind(auth_optional_entry_ids)
        .bind(now)
        .bind(now)
        .execute(&mut tx.executor())
        .await?;

        sqlx::query("DELETE FROM project_mcp_allowed_graphs WHERE config_id = $1")
            .bind(runtime.id)
            .execute(&mut tx.executor())
            .await?;

        for eid in &runtime.allowed_entry_ids {
            if eid.is_empty() {
                continue;
            }
            sqlx::query(
                r#"INSERT INTO project_mcp_allowed_graphs (id, config_id, entry_id, enabled)
                   VALUES ($1, $2, $3, true)"#,
            )
            .bind(Uuid::new_v4())
            .bind(runtime.id)
            .bind(eid)
            .execute(&mut tx.executor())
            .await?;
        }

        sqlx::query("DELETE FROM project_mcp_allowed_capabilities WHERE config_id = $1")
            .bind(runtime.id)
            .execute(&mut tx.executor())
            .await?;

        for (entry_id, names) in &runtime.capabilities_by_entry {
            if entry_id.is_empty() {
                continue;
            }
            for cap in names {
                if cap.is_empty() {
                    continue;
                }
                sqlx::query(
                    r#"INSERT INTO project_mcp_allowed_capabilities
                       (id, config_id, entry_id, capability_name, enabled)
                       VALUES ($1, $2, $3, $4, true)"#,
                )
                .bind(Uuid::new_v4())
                .bind(runtime.id)
                .bind(entry_id)
                .bind(cap)
                .execute(&mut tx.executor())
                .await?;
            }
        }

        sqlx::query("DELETE FROM project_mcp_auth_bindings WHERE config_id = $1")
            .bind(runtime.id)
            .execute(&mut tx.executor())
            .await?;

        for (entry_id, auth_id) in &runtime.auth_config_by_entry {
            if entry_id.is_empty() {
                continue;
            }
            sqlx::query(
                r#"INSERT INTO project_mcp_auth_bindings (id, config_id, entry_id, auth_config_id, inserted_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, $6)"#,
            )
            .bind(Uuid::new_v4())
            .bind(runtime.id)
            .bind(entry_id)
            .bind(auth_id)
            .bind(now)
            .bind(now)
            .execute(&mut tx.executor())
            .await?;
        }

        sqlx::query("DELETE FROM project_mcp_credentials WHERE config_id = $1")
            .bind(runtime.id)
            .execute(&mut tx.executor())
            .await?;

        for hash in &runtime.credential_secret_hashes {
            sqlx::query(
                r#"INSERT INTO project_mcp_credentials
                   (id, config_id, kind, secret_hash, status, inserted_at, updated_at)
                   VALUES ($1, $2, 'bearer', $3, 'active', $4, $5)"#,
            )
            .bind(Uuid::new_v4())
            .bind(runtime.id)
            .bind(&hash[..])
            .bind(now)
            .bind(now)
            .execute(&mut tx.executor())
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn revoke_config(&self, id: Uuid) -> Result<(), McpConfigRepositoryError> {
        sqlx::query("DELETE FROM project_mcp_configs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn config_exists(&self, id: &Uuid) -> Result<bool, sqlx::Error> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM project_mcp_configs WHERE id = $1")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        Ok(n > 0)
    }

    pub async fn set_mcp_api_key_fingerprint(
        &self,
        config_id: Uuid,
        fingerprint: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE project_mcp_configs SET mcp_api_key_fingerprint = $2, updated_at = $3 WHERE id = $1",
        )
        .bind(config_id)
        .bind(fingerprint)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_config_detail_json(&self, id: &Uuid) -> Result<Option<Value>, sqlx::Error> {
        let row = sqlx::query(
            r#"SELECT id, tenant_id, workspace_slug, project_slug, space_type, owner_subject,
                      name, status, config_version, endpoint_secret_hash, mcp_api_key_fingerprint,
                      auth_optional_entry_ids, inserted_at, updated_at
               FROM project_mcp_configs WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let config_id: Uuid = row.get("id");

        let graph_rows = sqlx::query(
            "SELECT entry_id, enabled FROM project_mcp_allowed_graphs WHERE config_id = $1 ORDER BY entry_id",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let graphs = graph_rows_to_json(graph_rows);

        let cap_rows = sqlx::query(
            "SELECT entry_id, capability_name, enabled FROM project_mcp_allowed_capabilities WHERE config_id = $1 ORDER BY entry_id, capability_name",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let caps = cap_rows_to_json(cap_rows);

        let bind_rows = sqlx::query(
            "SELECT entry_id, auth_config_id::text AS auth_config_id FROM project_mcp_auth_bindings WHERE config_id = $1 ORDER BY entry_id",
        )
        .bind(config_id)
        .fetch_all(&self.pool)
        .await?;
        let bindings = binding_rows_to_json(bind_rows);

        Ok(Some(config_detail_json_from_parts(
            &row, graphs, caps, bindings,
        )))
    }

    pub async fn list_configs_by_scope_json(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
        filter_space_type: Option<&str>,
        filter_owner_subject: Option<&str>,
    ) -> Result<Value, sqlx::Error> {
        let ids: Vec<Uuid> = if let (Some(st), Some(os)) = (filter_space_type, filter_owner_subject)
        {
            sqlx::query_scalar(
                r#"SELECT id FROM project_mcp_configs
                   WHERE tenant_id = $1 AND workspace_slug = $2 AND project_slug = $3
                     AND space_type = $4 AND owner_subject = $5
                   ORDER BY inserted_at DESC"#,
            )
            .bind(tenant_id)
            .bind(workspace_slug)
            .bind(project_slug)
            .bind(st)
            .bind(os)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_scalar(
                r#"SELECT id FROM project_mcp_configs
                   WHERE tenant_id = $1 AND workspace_slug = $2 AND project_slug = $3
                   ORDER BY inserted_at DESC"#,
            )
            .bind(tenant_id)
            .bind(workspace_slug)
            .bind(project_slug)
            .fetch_all(&self.pool)
            .await?
        };

        if ids.is_empty() {
            return Ok(json!({ "configs": [] }));
        }

        let mut qb = QueryBuilder::<Postgres>::new(
            r#"SELECT id, tenant_id, workspace_slug, project_slug, space_type, owner_subject,
                      name, status, config_version, endpoint_secret_hash, mcp_api_key_fingerprint,
                      auth_optional_entry_ids, inserted_at, updated_at
               FROM project_mcp_configs WHERE id IN ("#,
        );
        {
            let mut separated = qb.separated(", ");
            for id in &ids {
                separated.push_bind(id);
            }
        }
        qb.push(")");
        let config_rows = qb.build().fetch_all(&self.pool).await?;
        let mut by_id: HashMap<Uuid, sqlx::postgres::PgRow> =
            HashMap::with_capacity(config_rows.len());
        for row in config_rows {
            let id: Uuid = row.get("id");
            by_id.insert(id, row);
        }

        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT config_id, entry_id, enabled FROM project_mcp_allowed_graphs WHERE config_id IN (",
        );
        {
            let mut separated = qb.separated(", ");
            for id in &ids {
                separated.push_bind(id);
            }
        }
        qb.push(") ORDER BY config_id, entry_id");
        let graph_rows = qb.build().fetch_all(&self.pool).await?;
        let mut graphs_by = group_graph_json_rows(graph_rows);

        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT config_id, entry_id, capability_name, enabled FROM project_mcp_allowed_capabilities WHERE config_id IN (",
        );
        {
            let mut separated = qb.separated(", ");
            for id in &ids {
                separated.push_bind(id);
            }
        }
        qb.push(") ORDER BY config_id, entry_id, capability_name");
        let cap_rows = qb.build().fetch_all(&self.pool).await?;
        let mut caps_by = group_cap_json_rows(cap_rows);

        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT config_id, entry_id, auth_config_id::text AS auth_config_id FROM project_mcp_auth_bindings WHERE config_id IN (",
        );
        {
            let mut separated = qb.separated(", ");
            for id in &ids {
                separated.push_bind(id);
            }
        }
        qb.push(") ORDER BY config_id, entry_id");
        let bind_rows = qb.build().fetch_all(&self.pool).await?;
        let mut bindings_by = group_binding_json_rows(bind_rows);

        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = by_id.remove(&id) {
                let graphs = graphs_by.remove(&id).unwrap_or_default();
                let caps = caps_by.remove(&id).unwrap_or_default();
                let bindings = bindings_by.remove(&id).unwrap_or_default();
                items.push(config_detail_json_from_parts(&row, graphs, caps, bindings));
            }
        }

        Ok(json!({ "configs": items }))
    }
}

fn config_detail_json_from_parts(
    row: &sqlx::postgres::PgRow,
    graphs: Vec<Value>,
    caps: Vec<Value>,
    bindings: Vec<Value>,
) -> Value {
    let config_id: Uuid = row.get("id");
    let endpoint_hash: Vec<u8> = row.get("endpoint_secret_hash");
    let endpoint_hex = hex::encode(&endpoint_hash);
    let optional: Vec<String> = row.get::<Vec<String>, _>("auth_optional_entry_ids");
    json!({
        "id": config_id.to_string(),
        "tenant_id": row.get::<String, _>("tenant_id"),
        "workspace_slug": row.get::<String, _>("workspace_slug"),
        "project_slug": row.get::<String, _>("project_slug"),
        "space_type": row.get::<String, _>("space_type"),
        "owner_subject": row.get::<Option<String>, _>("owner_subject"),
        "name": row.get::<String, _>("name"),
        "status": row.get::<String, _>("status"),
        "config_version": row.get::<i64, _>("config_version"),
        "endpoint_secret_hash_hex": endpoint_hex,
        "mcp_api_key_fingerprint": row.get::<Option<String>, _>("mcp_api_key_fingerprint"),
        "auth_optional_entry_ids": optional,
        "allowed_graphs": graphs,
        "allowed_capabilities": caps,
        "auth_bindings": bindings,
        "inserted_at": row.get::<chrono::DateTime<Utc>, _>("inserted_at").to_rfc3339(),
        "updated_at": row.get::<chrono::DateTime<Utc>, _>("updated_at").to_rfc3339(),
    })
}

fn graph_rows_to_json(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| {
            json!({
                "entry_id": r.get::<String, _>("entry_id"),
                "enabled": r.get::<bool, _>("enabled"),
            })
        })
        .collect()
}

fn cap_rows_to_json(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| {
            json!({
                "entry_id": r.get::<String, _>("entry_id"),
                "capability_name": r.get::<String, _>("capability_name"),
                "enabled": r.get::<bool, _>("enabled"),
            })
        })
        .collect()
}

fn binding_rows_to_json(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| {
            json!({
                "entry_id": r.get::<String, _>("entry_id"),
                "auth_config_id": r.get::<String, _>("auth_config_id"),
            })
        })
        .collect()
}

fn group_graph_json_rows(rows: Vec<sqlx::postgres::PgRow>) -> HashMap<Uuid, Vec<Value>> {
    let mut m: HashMap<Uuid, Vec<Value>> = HashMap::new();
    for r in rows {
        let cid: Uuid = r.get("config_id");
        m.entry(cid).or_default().push(json!({
            "entry_id": r.get::<String, _>("entry_id"),
            "enabled": r.get::<bool, _>("enabled"),
        }));
    }
    m
}

fn group_cap_json_rows(rows: Vec<sqlx::postgres::PgRow>) -> HashMap<Uuid, Vec<Value>> {
    let mut m: HashMap<Uuid, Vec<Value>> = HashMap::new();
    for r in rows {
        let cid: Uuid = r.get("config_id");
        m.entry(cid).or_default().push(json!({
            "entry_id": r.get::<String, _>("entry_id"),
            "capability_name": r.get::<String, _>("capability_name"),
            "enabled": r.get::<bool, _>("enabled"),
        }));
    }
    m
}

fn group_binding_json_rows(rows: Vec<sqlx::postgres::PgRow>) -> HashMap<Uuid, Vec<Value>> {
    let mut m: HashMap<Uuid, Vec<Value>> = HashMap::new();
    for r in rows {
        let cid: Uuid = r.get("config_id");
        m.entry(cid).or_default().push(json!({
            "entry_id": r.get::<String, _>("entry_id"),
            "auth_config_id": r.get::<String, _>("auth_config_id"),
        }));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::{effective_owner_subject_for_hosted_kv, normalize_graph_binding_owner_subject};
    use uuid::Uuid;

    #[test]
    fn normalize_graph_binding_owner_subject_trims_and_drops_blank() {
        assert_eq!(normalize_graph_binding_owner_subject(None), None);
        assert_eq!(normalize_graph_binding_owner_subject(Some("")), None);
        assert_eq!(normalize_graph_binding_owner_subject(Some("   ")), None);
        assert_eq!(
            normalize_graph_binding_owner_subject(Some(" gh-1 ")),
            Some("gh-1")
        );
    }

    #[test]
    fn effective_owner_subject_ignores_synthetic_api_key_config_uuid() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        // Config row has no owner_subject; principal.subject is the synthetic config id (API key path).
        assert_eq!(
            effective_owner_subject_for_hosted_kv(id, None, Some(&id.to_string())),
            None,
            "synthetic config UUID subject must use unscoped hosted_kv lookup"
        );
    }

    #[test]
    fn effective_owner_subject_prefers_config_row_owner() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            effective_owner_subject_for_hosted_kv(id, Some("  gh-99  "), Some(&id.to_string())),
            Some("gh-99")
        );
    }

    #[test]
    fn effective_owner_subject_keeps_real_principal_when_no_config_owner() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            effective_owner_subject_for_hosted_kv(id, None, Some("oauth-subject-1")),
            Some("oauth-subject-1")
        );
    }

    #[test]
    fn squashed_migration_and_allowlisted_incrementals() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("migrations dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "sql"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(
            names.iter().any(|n| n.contains("plasm_agent_schema")),
            "expected squashed plasm_agent_schema migration, got {names:?}"
        );
        let allowlisted_incrementals = [
            "20260607000000_project_mcp_entry_bindings.sql",
            "20260706160000_project_flow_policies.sql",
        ];
        for name in &names {
            if name.contains("plasm_agent_schema") {
                continue;
            }
            assert!(
                allowlisted_incrementals.contains(&name.as_str()),
                "unexpected migration file {name}; add to allowlisted_incrementals if intentional"
            );
        }
    }
}
