//! In-process service helpers for `plasm-server` and other local UIs. These wrap [`PlasmHostState`]
//! without HTTP — keep logic shallow and delegate to existing runtime/catalog stores.

use plasm_core::discovery::CatalogEntryMeta;
use plasm_core::discovery::CgsCatalog;

use crate::server_state::PlasmHostState;

/// Catalog entries from the current registry snapshot (respects catalog-dir reload generation).
pub fn list_catalog_entries(state: &PlasmHostState) -> Vec<CatalogEntryMeta> {
    state.catalog.snapshot().list_entries()
}

/// Whether MCP policy sqlx + internal config routes are active.
pub fn mcp_policy_store_enabled(state: &PlasmHostState) -> bool {
    state.mcp_config_repository().is_some()
}

/// Compact trace hub bounds for status displays.
pub fn trace_hub_bounds_summary(state: &PlasmHostState) -> String {
    let b = &state.trace_hub_config.bounds;
    format!(
        "completed_max={} sse_cap={} ingest_cap={} timeline_max={}",
        b.max_completed_traces,
        b.sse_broadcast_capacity,
        b.ingest_queue_capacity,
        b.max_timeline_events
    )
}
