//! Filter discovery output using [`crate::mcp_runtime_config::McpRuntimeConfig`].

use crate::mcp_runtime_config::McpRuntimeConfig;

pub fn filter_registry_entries(
    entries: Vec<plasm_core::discovery::CatalogEntryMeta>,
    cfg: &McpRuntimeConfig,
) -> Vec<plasm_core::discovery::CatalogEntryMeta> {
    entries
        .into_iter()
        .filter(|m| cfg.entry_allowed(&m.entry_id))
        .collect()
}
