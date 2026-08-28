//! Postgres-backed mapping from incoming-auth `subject` (e.g. `github:<uid>`) to tenant + shell slugs.

use crate::traced_pg::PgPool;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

/// Same URL resolution as outbound OAuth provider pull (Phoenix `DATABASE_URL` in k8s).
pub fn tenant_binding_database_url() -> Option<String> {
    crate::oauth_provider_pull::outbound_oauth_provider_database_url()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TenantBindingRow {
    pub subject: String,
    pub tenant_id: String,
    pub workspace_slug: String,
    pub project_slug: String,
    pub workspace_name: String,
    pub project_name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OrgWorkspaceRow {
    pub subject: String,
    pub tenant_id: String,
    pub workspace_slug: String,
    pub workspace_name: String,
    pub membership_role: String,
}

pub struct TenantBindingStore {
    pool: PgPool,
}

fn slugify_login(login: &str) -> String {
    let lower = login.trim().to_lowercase();
    let mut out = String::new();
    let mut prev_hyphen = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_hyphen = false;
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !prev_hyphen && !out.is_empty() {
            out.push('-');
            prev_hyphen = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.len() > 48 {
        s.chars()
            .take(48)
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    } else {
        s
    }
}

fn tenant_id_for_github_subject(subject: &str) -> Option<String> {
    let rest = subject.strip_prefix("github:")?;
    let uid = rest.trim();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("gh-{uid}"))
}

/// Stable, collision-resistant id for non-GitHub subjects. Slugifying the raw `subject` maps many
/// distinct strings to the same `tenant_id` (e.g. `a@b` vs `a.b`), which then violates the table's
/// UNIQUE(`tenant_id`) when `ON CONFLICT (subject)` does not apply.
fn tenant_id_for_opaque_subject(subject: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(subject.as_bytes());
    let digest = hasher.finalize();
    format!("sub-{}", hex::encode(digest))
}

fn provisional_slugs(
    subject: &str,
    github_login: Option<&str>,
) -> (String, String, String, String, String) {
    let tenant_id = tenant_id_for_github_subject(subject)
        .unwrap_or_else(|| tenant_id_for_opaque_subject(subject));

    let base = github_login.map(slugify_login).unwrap_or_default();
    let workspace_slug = if base.is_empty() {
        tenant_id.clone()
    } else {
        base
    };

    let workspace_name = match github_login {
        Some(l) if !l.trim().is_empty() => format!("{} · workspace", l.trim()),
        _ => format!("Workspace · {}", tenant_id),
    };

    let project_slug = "main".to_string();
    let project_name = "Main".to_string();

    (
        tenant_id,
        workspace_slug,
        workspace_name,
        project_slug,
        project_name,
    )
}

impl TenantBindingStore {
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect(database_url)
            .await?;
        Self::ensure_schema(&pool).await?;
        let pool = crate::traced_pg::wrap(pool);
        Ok(Self { pool })
    }

    async fn ensure_schema(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
CREATE TABLE IF NOT EXISTS plasm_incoming_subject_binding (
    subject TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL UNIQUE,
    workspace_slug TEXT NOT NULL,
    project_slug TEXT NOT NULL,
    workspace_name TEXT NOT NULL,
    project_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#,
        )
        .execute(pool)
        .await?;
        sqlx::query(
            r#"
CREATE INDEX IF NOT EXISTS plasm_incoming_subject_binding_tenant_id_idx
    ON plasm_incoming_subject_binding (tenant_id);
"#,
        )
        .execute(pool)
        .await?;
        sqlx::query(
            r#"
CREATE TABLE IF NOT EXISTS plasm_incoming_org_workspace (
    subject TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    workspace_slug TEXT NOT NULL,
    workspace_name TEXT NOT NULL,
    membership_role TEXT NOT NULL DEFAULT 'owner',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (subject, workspace_slug)
);
"#,
        )
        .execute(pool)
        .await?;
        sqlx::query(
            r#"
CREATE UNIQUE INDEX IF NOT EXISTS plasm_incoming_org_workspace_tenant_slug_idx
    ON plasm_incoming_org_workspace (tenant_id, workspace_slug);
"#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    fn row_from_sql(row: &sqlx::postgres::PgRow) -> Result<TenantBindingRow, sqlx::Error> {
        Ok(TenantBindingRow {
            subject: row.try_get("subject")?,
            tenant_id: row.try_get("tenant_id")?,
            workspace_slug: row.try_get("workspace_slug")?,
            project_slug: row.try_get("project_slug")?,
            workspace_name: row.try_get("workspace_name")?,
            project_name: row.try_get("project_name")?,
        })
    }

    fn org_row_from_sql(row: &sqlx::postgres::PgRow) -> Result<OrgWorkspaceRow, sqlx::Error> {
        Ok(OrgWorkspaceRow {
            subject: row.try_get("subject")?,
            tenant_id: row.try_get("tenant_id")?,
            workspace_slug: row.try_get("workspace_slug")?,
            workspace_name: row.try_get("workspace_name")?,
            membership_role: row.try_get("membership_role")?,
        })
    }

    pub async fn get_by_subject(
        &self,
        subject: &str,
    ) -> Result<Option<TenantBindingRow>, sqlx::Error> {
        let row = sqlx::query(
            r#"
SELECT subject, tenant_id, workspace_slug, project_slug, workspace_name, project_name
FROM plasm_incoming_subject_binding
WHERE subject = $1
"#,
        )
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Self::row_from_sql(&r)).transpose()
    }

    /// Idempotent: return existing row or insert a new binding for this subject.
    pub async fn resolve_or_insert(
        &self,
        subject: &str,
        github_login: Option<&str>,
    ) -> Result<TenantBindingRow, sqlx::Error> {
        let (tenant_id, workspace_slug, workspace_name, project_slug, project_name) =
            provisional_slugs(subject, github_login);

        let row = sqlx::query(
            r#"
INSERT INTO plasm_incoming_subject_binding
    (subject, tenant_id, workspace_slug, project_slug, workspace_name, project_name)
VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT (subject) DO UPDATE SET tenant_id = plasm_incoming_subject_binding.tenant_id
RETURNING subject, tenant_id, workspace_slug, project_slug, workspace_name, project_name
"#,
        )
        .bind(subject)
        .bind(&tenant_id)
        .bind(&workspace_slug)
        .bind(&project_slug)
        .bind(&workspace_name)
        .bind(&project_name)
        .fetch_one(&self.pool)
        .await?;

        Self::row_from_sql(&row)
    }

    pub async fn list_org_workspaces(
        &self,
        subject: &str,
        tenant_id: &str,
    ) -> Result<Vec<OrgWorkspaceRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
SELECT subject, tenant_id, workspace_slug, workspace_name, membership_role
FROM plasm_incoming_org_workspace
WHERE subject = $1 AND tenant_id = $2
ORDER BY created_at DESC
"#,
        )
        .bind(subject)
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(Self::org_row_from_sql).collect()
    }

    pub async fn create_org_workspace(
        &self,
        subject: &str,
        tenant_id: &str,
        workspace_slug: &str,
        workspace_name: &str,
    ) -> Result<OrgWorkspaceRow, sqlx::Error> {
        let row = sqlx::query(
            r#"
INSERT INTO plasm_incoming_org_workspace
    (subject, tenant_id, workspace_slug, workspace_name, membership_role)
VALUES ($1, $2, $3, $4, 'owner')
RETURNING subject, tenant_id, workspace_slug, workspace_name, membership_role
"#,
        )
        .bind(subject)
        .bind(tenant_id)
        .bind(workspace_slug)
        .bind(workspace_name)
        .fetch_one(&self.pool)
        .await?;

        Self::org_row_from_sql(&row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_github_subject_with_login_slugifies_workspace() {
        let (tid, ws, wn, ps, pn) = provisional_slugs("github:7", Some("Cool_Dev"));
        assert_eq!(tid, "gh-7");
        assert_eq!(ws, "cool-dev");
        assert_eq!(wn, "Cool_Dev · workspace");
        assert_eq!(ps, "main");
        assert_eq!(pn, "Main");
    }

    #[test]
    fn provisional_github_subject_without_login_uses_tenant_slug() {
        let (tid, ws, _wn, ps, pn) = provisional_slugs("github:99", None);
        assert_eq!(tid, "gh-99");
        assert_eq!(ws, "gh-99");
        assert_eq!(ps, "main");
        assert_eq!(pn, "Main");
    }

    #[test]
    fn provisional_opaque_subjects_that_slug_collision_do_not_share_tenant_id() {
        let (a, ..) = provisional_slugs("email:user@foo", None);
        let (b, ..) = provisional_slugs("email:user.foo", None);
        assert_ne!(a, b);
    }

    #[test]
    fn provisional_opaque_subject_tenant_id_is_stable() {
        let (a, ..) = provisional_slugs("oidc:acme|sub|xyz", None);
        let (b, ..) = provisional_slugs("oidc:acme|sub|xyz", None);
        assert_eq!(a, b);
        assert!(a.starts_with("sub-"));
        assert_eq!(a.len(), 4 + 64);
    }
}
