//! sqlx-backed persistence for project-scoped plan flow policies.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use thiserror::Error;

use crate::plan_flow_policy::{FlowPolicy, FlowPolicySnapshot, PolicyRevision};

pub use crate::mcp_config_repository::mcp_config_database_url;

#[derive(Debug, Error)]
pub enum FlowPolicyRepositoryError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    InvalidInput(String),
    #[error("no draft to publish")]
    NoDraft,
    #[error("validate required before publish")]
    ValidateRequired,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FlowPolicyRow {
    pub tenant_id: String,
    pub workspace_slug: String,
    pub project_slug: String,
    pub published_revision: u64,
    pub published_policy: Option<FlowPolicy>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by_subject: Option<String>,
    pub draft_policy: Option<FlowPolicy>,
    pub draft_updated_at: Option<DateTime<Utc>>,
    pub draft_validated_at: Option<DateTime<Utc>>,
    pub draft_validation_ok: Option<bool>,
}

impl FlowPolicyRow {
    pub fn published_snapshot(&self) -> FlowPolicySnapshot {
        match (&self.published_policy, self.published_revision) {
            (Some(policy), rev) if rev > 0 => FlowPolicySnapshot::Active {
                revision: PolicyRevision(rev),
                policy: policy.clone(),
            },
            _ => FlowPolicySnapshot::inactive_default(),
        }
    }
}

#[derive(Clone)]
pub struct FlowPolicyRepository {
    pool: PgPool,
}

impl FlowPolicyRepository {
    pub async fn connect_and_migrate(
        database_url: &str,
    ) -> Result<Self, FlowPolicyRepositoryError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Share an existing MCP config pool (migrations already applied on that database).
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn get_or_default(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
    ) -> Result<FlowPolicyRow, FlowPolicyRepositoryError> {
        let row = sqlx::query(
            r#"SELECT tenant_id, workspace_slug, project_slug,
                      published_revision, published_policy_json,
                      published_at, published_by_subject,
                      draft_policy_json, draft_updated_at,
                      draft_validated_at, draft_validation_ok
               FROM project_flow_policies
               WHERE tenant_id = $1 AND workspace_slug = $2 AND project_slug = $3"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some(r) => row_to_policy(r)?,
            None => FlowPolicyRow {
                tenant_id: tenant_id.to_string(),
                workspace_slug: workspace_slug.to_string(),
                project_slug: project_slug.to_string(),
                published_revision: 0,
                published_policy: None,
                published_at: None,
                published_by_subject: None,
                draft_policy: None,
                draft_updated_at: None,
                draft_validated_at: None,
                draft_validation_ok: None,
            },
        })
    }

    pub async fn upsert_draft(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
        policy: &FlowPolicy,
    ) -> Result<(), FlowPolicyRepositoryError> {
        let json = serde_json::to_value(policy).map_err(|e| {
            FlowPolicyRepositoryError::InvalidInput(format!("policy serialize: {e}"))
        })?;
        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO project_flow_policies (
                tenant_id, workspace_slug, project_slug,
                draft_policy_json, draft_updated_at,
                draft_validated_at, draft_validation_ok,
                inserted_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, NULL, NULL, $5, $5)
            ON CONFLICT (tenant_id, workspace_slug, project_slug) DO UPDATE SET
                draft_policy_json = EXCLUDED.draft_policy_json,
                draft_updated_at = EXCLUDED.draft_updated_at,
                draft_validated_at = NULL,
                draft_validation_ok = NULL,
                updated_at = EXCLUDED.updated_at"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .bind(json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_validation(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
        ok: bool,
    ) -> Result<(), FlowPolicyRepositoryError> {
        let now = Utc::now();
        sqlx::query(
            r#"UPDATE project_flow_policies
               SET draft_validated_at = $4, draft_validation_ok = $5, updated_at = $4
               WHERE tenant_id = $1 AND workspace_slug = $2 AND project_slug = $3"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .bind(now)
        .bind(ok)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
        published_by_subject: Option<&str>,
    ) -> Result<u64, FlowPolicyRepositoryError> {
        let row = self
            .get_or_default(tenant_id, workspace_slug, project_slug)
            .await?;
        let Some(draft) = row.draft_policy else {
            return Err(FlowPolicyRepositoryError::NoDraft);
        };
        if !row.draft_validation_ok.unwrap_or(false) {
            return Err(FlowPolicyRepositoryError::ValidateRequired);
        }
        let next_rev = row.published_revision.saturating_add(1).max(1);
        let json = serde_json::to_value(&draft).map_err(|e| {
            FlowPolicyRepositoryError::InvalidInput(format!("policy serialize: {e}"))
        })?;
        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO project_flow_policies (
                tenant_id, workspace_slug, project_slug,
                published_revision, published_policy_json,
                published_at, published_by_subject,
                draft_policy_json, draft_updated_at,
                draft_validated_at, draft_validation_ok,
                inserted_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $5, $6, $6, true, $6, $6)
            ON CONFLICT (tenant_id, workspace_slug, project_slug) DO UPDATE SET
                published_revision = EXCLUDED.published_revision,
                published_policy_json = EXCLUDED.published_policy_json,
                published_at = EXCLUDED.published_at,
                published_by_subject = EXCLUDED.published_by_subject,
                draft_policy_json = EXCLUDED.draft_policy_json,
                draft_updated_at = EXCLUDED.draft_updated_at,
                draft_validated_at = EXCLUDED.draft_validated_at,
                draft_validation_ok = EXCLUDED.draft_validation_ok,
                updated_at = EXCLUDED.updated_at"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .bind(next_rev as i64)
        .bind(json)
        .bind(now)
        .bind(published_by_subject)
        .execute(&self.pool)
        .await?;
        Ok(next_rev)
    }

    pub async fn discard_draft(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
    ) -> Result<(), FlowPolicyRepositoryError> {
        let now = Utc::now();
        sqlx::query(
            r#"UPDATE project_flow_policies
               SET draft_policy_json = NULL,
                   draft_updated_at = NULL,
                   draft_validated_at = NULL,
                   draft_validation_ok = NULL,
                   updated_at = $4
               WHERE tenant_id = $1 AND workspace_slug = $2 AND project_slug = $3"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn enabled_entry_ids_for_project(
        &self,
        tenant_id: &str,
        workspace_slug: &str,
        project_slug: &str,
    ) -> Result<Vec<String>, FlowPolicyRepositoryError> {
        let rows = sqlx::query(
            r#"SELECT DISTINCT g.entry_id
               FROM project_mcp_configs c
               JOIN project_mcp_allowed_graphs g ON g.config_id = c.id
               WHERE c.tenant_id = $1 AND c.workspace_slug = $2 AND c.project_slug = $3
                 AND c.status = 'active' AND g.enabled = true
               ORDER BY g.entry_id"#,
        )
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(project_slug)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("entry_id"))
            .collect())
    }
}

fn row_to_policy(row: sqlx::postgres::PgRow) -> Result<FlowPolicyRow, FlowPolicyRepositoryError> {
    let published_json: Option<Value> = row.try_get("published_policy_json")?;
    let draft_json: Option<Value> = row.try_get("draft_policy_json")?;
    let published_policy = published_json
        .map(parse_policy_value)
        .transpose()?
        .flatten();
    let draft_policy = draft_json.map(parse_policy_value).transpose()?.flatten();
    Ok(FlowPolicyRow {
        tenant_id: row.get("tenant_id"),
        workspace_slug: row.get("workspace_slug"),
        project_slug: row.get("project_slug"),
        published_revision: row.get::<i64, _>("published_revision") as u64,
        published_policy,
        published_at: row.try_get("published_at")?,
        published_by_subject: row.try_get("published_by_subject")?,
        draft_policy,
        draft_updated_at: row.try_get("draft_updated_at")?,
        draft_validated_at: row.try_get("draft_validated_at")?,
        draft_validation_ok: row.try_get("draft_validation_ok")?,
    })
}

fn parse_policy_value(v: Value) -> Result<Option<FlowPolicy>, FlowPolicyRepositoryError> {
    if v.is_null() {
        return Ok(None);
    }
    serde_json::from_value(v).map(Some).map_err(|e| {
        FlowPolicyRepositoryError::InvalidInput(format!("stored policy JSON invalid: {e}"))
    })
}
