//! Rehydrate graph-backed rows from hot cache + spilled pages for plan compute.
//!
//! Row order: hot-cache entities first (type-index iteration order), then spill pages
//! sorted by `page_index`. Duplicates are suppressed by stable identity (`_ref`, or
//! `{entity_type}:{id_field}` when `_ref` is absent on a spill row).
//!
//! ## Concurrency (enforced)
//!
//! This module **never** acquires the session graph mutex directly. Hot entities are
//! copied under a brief [`GraphCacheGuard`] (or returned from [`GraphSpillSyncPlan`]);
//! **all spill / object-store I/O runs without the graph mutex held**.

mod ctx;
mod rehydrator;
mod relation_embed;
mod walk;

#[cfg(test)]
mod tests;

pub(crate) use rehydrator::GraphSurfaceRehydrator;
pub(crate) use relation_embed::{
    collect_all_embedded_relation_targets, plan_prefer_from_parent_get,
    wire_rows_for_embed_entities,
};
