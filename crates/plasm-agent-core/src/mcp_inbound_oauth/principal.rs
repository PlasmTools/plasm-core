use crate::incoming_auth::TenantPrincipal;
use crate::server_state::PlasmHostState;

use super::error::McpOAuthError;

pub async fn verify_authorization_principal(
    plasm: &PlasmHostState,
    principal_token: &str,
) -> Result<TenantPrincipal, McpOAuthError> {
    let verifier = plasm.incoming_auth.as_deref().ok_or_else(|| {
        McpOAuthError::unavailable("incoming auth is not configured for OAuth authorization")
    })?;
    let principal = verifier.verify_bearer_token(principal_token).map_err(|_| {
        McpOAuthError::AccessDenied {
            description: "principal token is invalid".to_string(),
        }
    })?;
    let Some(repo) = plasm.mcp_config_repository() else {
        return Err(McpOAuthError::unavailable("MCP policy store unavailable"));
    };
    let ok = repo
        .find_personal_runtime(&principal.tenant_id, &principal.subject)
        .await
        .map_err(|_| McpOAuthError::unavailable("MCP policy store error"))?
        .is_some();
    if !ok {
        return Err(McpOAuthError::AccessDenied {
            description:
                "principal is not bound to an active personal MCP configuration".to_string(),
        });
    }
    Ok(principal)
}
