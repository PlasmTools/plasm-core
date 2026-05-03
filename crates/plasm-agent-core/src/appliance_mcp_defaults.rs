//! Default synthetic tenant triple for a single-user OSS appliance (`project_mcp_configs` row).
//!
//! Override via `PLASM_APPLIANCE_MCP_*` so Phoenix/scripts align with the UUID they upsert.

/// `tenant_id` for the appliance MCP config when not overridden by env.
pub fn appliance_mcp_tenant_id() -> String {
    trimmed_env_or("PLASM_APPLIANCE_MCP_TENANT_ID", "appliance-local")
}

/// `workspace_slug` for the appliance MCP config when not overridden by env.
pub fn appliance_mcp_workspace_slug() -> String {
    trimmed_env_or("PLASM_APPLIANCE_MCP_WORKSPACE_SLUG", "default")
}

/// `project_slug` for the appliance MCP config when not overridden by env.
pub fn appliance_mcp_project_slug() -> String {
    trimmed_env_or("PLASM_APPLIANCE_MCP_PROJECT_SLUG", "default")
}

fn trimmed_env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Optional stable MCP config UUID shared with desktop (`PLASM_APPLIANCE_MCP_CONFIG_ID`); agent does not require it.
pub fn appliance_mcp_config_id_from_env() -> Option<String> {
    std::env::var("PLASM_APPLIANCE_MCP_CONFIG_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
