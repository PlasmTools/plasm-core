-- Per-MCP catalog binding pointers (encrypted values in AuthStorage KV).

CREATE TABLE IF NOT EXISTS project_mcp_entry_bindings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    config_id UUID NOT NULL REFERENCES project_mcp_configs (id) ON DELETE CASCADE,
    entry_id TEXT NOT NULL,
    binding_kv_key TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS project_mcp_entry_bindings_config_entry
    ON project_mcp_entry_bindings (config_id, entry_id);

CREATE INDEX IF NOT EXISTS project_mcp_entry_bindings_tenant_config
    ON project_mcp_entry_bindings (tenant_id, config_id);
