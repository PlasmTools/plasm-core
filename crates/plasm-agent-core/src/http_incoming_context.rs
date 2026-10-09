//! Shell context for UIs: principal + tenant-scoped workspace/project list (authoritative for Phoenix).

use axum::extract::Extension;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::incoming_auth::{
    incoming_auth_problem, IncomingAuthFailure, IncomingAuthMethod, IncomingAuthMode,
    IncomingPrincipal, TenantPrincipal,
};
use crate::server_state::PlasmHostState;
use crate::tenant_binding::{OrgWorkspaceRow, TenantBindingRow};

#[derive(Debug, Serialize)]
pub struct ShellWorkspace {
    pub slug: String,
    pub name: String,
    pub tenant_id: String,
    pub workspace_type: String,
    pub membership_role: String,
}

#[derive(Debug, Serialize)]
pub struct ShellProject {
    pub slug: String,
    pub name: String,
    /// Owning workspace slug (Phoenix uses this to bind `/w/:workspace_slug/projects/:project_slug/...`).
    pub workspace_slug: String,
    pub workspace_type: String,
}

#[derive(Debug, Serialize)]
pub struct IncomingAuthContextResponse {
    pub tenant_id: String,
    pub subject: String,
    pub auth_method: &'static str,
    pub default_workspace_slug: Option<String>,
    pub workspaces: Vec<ShellWorkspace>,
    pub projects: Vec<ShellProject>,
}

fn slug_from_tenant_id(tenant_id: &str) -> String {
    let s: String = tenant_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "tenant".to_string()
    } else {
        s
    }
}

fn build_context(p: &TenantPrincipal) -> IncomingAuthContextResponse {
    let org_slug = slug_from_tenant_id(&p.tenant_id);
    let personal_slug = format!("{org_slug}-personal");
    let auth_method = match p.method {
        IncomingAuthMethod::Jwt => "jwt",
        IncomingAuthMethod::ApiKey => "api_key",
    };
    IncomingAuthContextResponse {
        tenant_id: p.tenant_id.clone(),
        subject: p.subject.clone(),
        auth_method,
        default_workspace_slug: Some(personal_slug.clone()),
        workspaces: vec![
            ShellWorkspace {
                slug: personal_slug.clone(),
                name: "Personal".to_string(),
                tenant_id: p.tenant_id.clone(),
                workspace_type: "personal".to_string(),
                membership_role: "owner".to_string(),
            },
            ShellWorkspace {
                slug: org_slug.clone(),
                name: format!("Organization · {}", p.tenant_id),
                tenant_id: p.tenant_id.clone(),
                workspace_type: "organization".to_string(),
                membership_role: "owner".to_string(),
            },
        ],
        projects: vec![
            ShellProject {
                slug: "your-mcp".to_string(),
                name: "Your MCP".to_string(),
                workspace_slug: personal_slug,
                workspace_type: "personal".to_string(),
            },
            ShellProject {
                slug: "main".to_string(),
                name: "Main".to_string(),
                workspace_slug: org_slug,
                workspace_type: "organization".to_string(),
            },
        ],
    }
}

fn build_from_binding(
    p: &TenantPrincipal,
    row: &TenantBindingRow,
    extra_orgs: &[OrgWorkspaceRow],
) -> IncomingAuthContextResponse {
    let personal_slug = format!("{}-personal", row.workspace_slug);
    let auth_method = match p.method {
        IncomingAuthMethod::Jwt => "jwt",
        IncomingAuthMethod::ApiKey => "api_key",
    };
    let mut workspaces = vec![
        ShellWorkspace {
            slug: personal_slug.clone(),
            name: "Personal".to_string(),
            tenant_id: p.tenant_id.clone(),
            workspace_type: "personal".to_string(),
            membership_role: "owner".to_string(),
        },
        ShellWorkspace {
            slug: row.workspace_slug.clone(),
            name: row.workspace_name.clone(),
            tenant_id: p.tenant_id.clone(),
            workspace_type: "organization".to_string(),
            membership_role: "owner".to_string(),
        },
    ];
    let mut projects = vec![
        ShellProject {
            slug: "your-mcp".to_string(),
            name: "Your MCP".to_string(),
            workspace_slug: personal_slug.clone(),
            workspace_type: "personal".to_string(),
        },
        ShellProject {
            slug: row.project_slug.clone(),
            name: row.project_name.clone(),
            workspace_slug: row.workspace_slug.clone(),
            workspace_type: "organization".to_string(),
        },
    ];

    for org in extra_orgs {
        if org.workspace_slug == row.workspace_slug {
            continue;
        }
        workspaces.push(ShellWorkspace {
            slug: org.workspace_slug.clone(),
            name: org.workspace_name.clone(),
            tenant_id: p.tenant_id.clone(),
            workspace_type: "organization".to_string(),
            membership_role: org.membership_role.clone(),
        });
        projects.push(ShellProject {
            slug: "main".to_string(),
            name: "Main".to_string(),
            workspace_slug: org.workspace_slug.clone(),
            workspace_type: "organization".to_string(),
        });
    }

    IncomingAuthContextResponse {
        tenant_id: p.tenant_id.clone(),
        subject: p.subject.clone(),
        auth_method,
        default_workspace_slug: Some(personal_slug.clone()),
        workspaces,
        projects,
    }
}

fn anonymous() -> IncomingAuthContextResponse {
    IncomingAuthContextResponse {
        tenant_id: String::new(),
        subject: String::new(),
        auth_method: "anonymous",
        default_workspace_slug: None,
        workspaces: vec![],
        projects: vec![],
    }
}

/// `GET /v1/incoming-auth/context` — principal + shell workspace/project model (Rust-owned).
#[allow(
    clippy::result_large_err,
    reason = "HTTP boundary errors are fully rendered Axum responses"
)]
pub async fn get_incoming_auth_context(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
) -> Result<Json<IncomingAuthContextResponse>, Response> {
    if let Some(p) = principal {
        if let Some(store) = st.tenant_binding() {
            match store.get_by_subject(&p.subject).await {
                Ok(Some(row)) if row.tenant_id == p.tenant_id => {
                    let extras = store
                        .list_org_workspaces(&p.subject, &p.tenant_id)
                        .await
                        .unwrap_or_default();
                    return Ok(Json(build_from_binding(&p, &row, &extras)));
                }
                Ok(Some(row)) => {
                    tracing::warn!(
                        subject = %p.subject,
                        jwt_tenant = %p.tenant_id,
                        bound_tenant = %row.tenant_id,
                        "incoming-auth context: binding tenant mismatch; using synthetic shell"
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "incoming-auth context: binding lookup failed")
                }
            }
        }
        return Ok(Json(build_context(&p)));
    }

    match st.incoming_auth.as_ref() {
        None => Ok(Json(anonymous())),
        Some(v) if v.mode() == IncomingAuthMode::Off => Ok(Json(anonymous())),
        Some(_) => Err(incoming_auth_problem(IncomingAuthFailure::Missing, false).into_response()),
    }
}

pub fn incoming_context_routes() -> axum::Router {
    axum::Router::new().route(
        "/v1/incoming-auth/context",
        axum::routing::get(get_incoming_auth_context),
    )
}
