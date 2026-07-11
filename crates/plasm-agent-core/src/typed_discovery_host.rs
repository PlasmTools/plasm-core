//! Host wiring for [`plasm_discovery::TypedDiscovery`] over the in-memory CGS registry.

use plasm_core::discovery::{CgsCatalog, InMemoryCgsRegistry};
use plasm_discovery::{
    AgentDiscovery, CatalogIndexCache, DiscoveryDecision, DiscoveryQuery, TypedDiscovery,
};

/// Run typed discovery against the current catalog snapshot.
///
/// When `query.allowed_entry_ids` is empty, all registry entries are considered (subject to caller filtering).
pub async fn run_typed_catalog_discovery(
    reg: &InMemoryCgsRegistry,
    mut query: DiscoveryQuery,
    index_cache: Option<&CatalogIndexCache>,
) -> Result<DiscoveryDecision, plasm_discovery::DiscoveryError> {
    if query.allowed_entry_ids.is_empty() {
        query.allowed_entry_ids = reg.list_entries().into_iter().map(|m| m.entry_id).collect();
    }

    let mut entries = Vec::new();
    for eid in &query.allowed_entry_ids {
        let ctx = reg.load_context(eid)?;
        entries.push((eid.clone(), ctx.cgs));
    }

    if entries.is_empty() {
        return Ok(DiscoveryDecision::NoMatch {
            evidence: vec![plasm_discovery::DiscoveryEvidence::new(
                "no_catalog_entries",
                "allowed_entry_ids did not resolve to any loaded catalogs",
            )],
        });
    }

    let max = query.max_options;
    let disc = TypedDiscovery::from_cgs_entries(entries, index_cache).with_max_options(max);
    disc.discover(query).await
}
