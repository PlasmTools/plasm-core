//! Rehydrate graph-backed rows from hot cache + spilled pages for plan compute.
//!
//! Logical order and multiplicity come from the recording codec. Hot-cache and
//! spill lookup resolve each identity once; shared row views replay every recorded
//! occurrence. Missing identities are errors, including on partial delivery windows.
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
    collect_all_embedded_relation_targets, wire_rows_for_embed_entities,
};
