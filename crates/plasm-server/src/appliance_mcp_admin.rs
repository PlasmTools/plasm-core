//! OSS appliance adapter: synthetic tenant scope + wiring helpers for [`plasm_agent_core::mcp_config_admin`].

use std::sync::Arc;

use plasm_agent_core::appliance_mcp_defaults;
use plasm_agent_core::auth_framework_host;
use plasm_agent_core::mcp_api_key_registry::McpApiKeyRegistry;
use plasm_agent_core::mcp_config_admin::{McpConfigAdminService, McpConfigScope};
use plasm_agent_core::mcp_config_repository::McpConfigRepository;
use plasm_agent_core::mcp_transport_auth::McpTransportAuth;
use plasm_agent_core::server_state::PlasmHostState;
use uuid::Uuid;

/// Canonical appliance MCP scope (`organization` row under the synthetic tenant triple).
pub fn appliance_mcp_scope() -> McpConfigScope {
    McpConfigScope::organization_workspace_project(
        appliance_mcp_defaults::appliance_mcp_tenant_id(),
        appliance_mcp_defaults::appliance_mcp_workspace_slug(),
        appliance_mcp_defaults::appliance_mcp_project_slug(),
    )
}

pub fn appliance_preferred_config_id() -> Option<Uuid> {
    appliance_mcp_defaults::appliance_mcp_config_id_from_env()
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
}

pub fn admin_service_from_host(state: &PlasmHostState) -> Option<McpConfigAdminService> {
    let repo = state.mcp_config_repository()?.clone();
    let keys: Arc<dyn McpTransportAuth> = state.mcp_transport_auth()?.clone();
    Some(McpConfigAdminService::new(repo, keys))
}

#[derive(Debug, thiserror::Error)]
pub enum StandaloneMcpAdminError {
    #[error("policy store URL missing: set DATABASE_URL, PLASM_MCP_CONFIG_DATABASE_URL, or PLASM_AUTH_STORAGE_URL")]
    MissingPolicyStoreUrl,
    #[error("MCP policy store connection or migration failed")]
    Repository(#[from] plasm_agent_core::mcp_config_repository::McpConfigRepositoryError),
}

/// CLI / tooling: connect sqlx + MCP transport auth without booting HTTP listeners.
pub async fn connect_standalone_mcp_admin_service(
) -> Result<McpConfigAdminService, StandaloneMcpAdminError> {
    let Some(db_url) = plasm_agent_core::mcp_config_repository::mcp_config_database_url() else {
        return Err(StandaloneMcpAdminError::MissingPolicyStoreUrl);
    };
    let repo = Arc::new(McpConfigRepository::connect_and_migrate(&db_url).await?);
    let keys: Arc<dyn McpTransportAuth> =
        match auth_framework_host::init_standalone_auth_storage().await {
            Ok(storage) => Arc::new(McpApiKeyRegistry::new(storage)),
            Err(_) => auth_framework_host::mcp_api_key_registry_memory_only(),
        };
    Ok(McpConfigAdminService::new(repo, keys))
}
