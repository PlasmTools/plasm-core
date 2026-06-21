//! Unified graph-surface rehydrate API (plan / apply / materialize / stream).
//!
//! **CEP-4:** spill/rehydrate I/O runs without the session graph mutex held.
//! **CEP-5:** [`Self::resolve_source_parents`] keeps parent entities aligned with
//! [`MaterializedRowSource`] when [`ExecutionResult::entities`] is empty (GraphBacked).

use std::sync::Arc;

use plasm_core::CGS;
use plasm_runtime::{
    CachedEntity, ExecutionResult, MaterializedRowSource,
    SessionMaterialization,
};

use crate::execute_session::ExecuteSession;
use crate::server_state::PlasmHostState;

use super::ctx::GraphSurfaceWalkCtx;
use super::walk::{collect_entities, collect_row_json, snapshot_hot_entities, stream_rows};

/// Hot-cache snapshot + target count for spill rehydrate after the graph lock is released.
pub(crate) struct GraphSpillSyncPlan {
    pub hot_snapshot: Arc<[CachedEntity]>,
    pub entity_type: String,
    pub logical_count: usize,
    pub spill_enabled: bool,
}

/// Session-scoped rehydrator: all spill I/O runs without the graph mutex held.
pub(crate) struct GraphSurfaceRehydrator<'a> {
    ctx: GraphSurfaceWalkCtx<'a>,
}

impl<'a> GraphSurfaceRehydrator<'a> {
    pub(crate) fn new(
        es: &'a ExecuteSession,
        st: &'a PlasmHostState,
        session_id: &'a str,
        cgs: &'a CGS,
    ) -> Self {
        Self {
            ctx: GraphSurfaceWalkCtx::new(es, st, session_id, cgs),
        }
    }

    pub(crate) async fn snapshot_hot_locked(&self, entity_type: &str) -> Arc<[CachedEntity]> {
        let guard = self.ctx.es.lock_graph_cache().await;
        snapshot_hot_entities(guard.materialization(), entity_type)
    }

    /// Plan spill rehydrate from a graph-backed result (`count > 0`, empty `entities`).
    /// Does not perform I/O — safe while the graph lock is held.
    pub(crate) fn plan_spill_sync(
        hot: &SessionMaterialization,
        st: &PlasmHostState,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> Option<GraphSpillSyncPlan> {
        if !result.entities.is_empty() || result.count == 0 {
            return None;
        }
        Some(GraphSpillSyncPlan {
            hot_snapshot: snapshot_hot_entities(hot, entity_type),
            entity_type: entity_type.to_string(),
            logical_count: result.count,
            spill_enabled: st.session_graph_persistence.is_some(),
        })
    }

    /// Apply spill rehydrate without holding the graph mutex (may read object store).
    pub(crate) async fn apply_spill_sync(
        &self,
        plan: GraphSpillSyncPlan,
        result: &mut ExecutionResult,
    ) {
        let entities = collect_entities(
            &self.ctx,
            plan.hot_snapshot,
            plan.entity_type.as_str(),
            plan.spill_enabled,
            plan.logical_count,
        )
        .await
        .unwrap_or_default();
        if entities.is_empty() {
            return;
        }
        result.entities = entities;
        if result.count < result.entities.len() {
            result.count = result.entities.len();
        }
    }

    pub(crate) async fn materialize_surface_rows(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> MaterializedRowSource {
        if !result.entities.is_empty() {
            let guard = self.ctx.es.lock_graph_cache().await;
            let mat = guard.materialization();
            return MaterializedRowSource::Inline(
                result
                    .entities
                    .iter()
                    .map(|e| super::relation_embed::wire_row_with_from_parent_embeds(
                        e,
                        self.ctx.cgs,
                        &mat,
                    ))
                    .collect(),
            );
        }
        if result.count == 0 {
            return MaterializedRowSource::Inline(Vec::new());
        }

        let spill_enabled = self.ctx.spill_enabled();
        let hot_snapshot = self.snapshot_hot_locked(entity_type).await;

        if !spill_enabled || hot_snapshot.len() >= result.count {
            let rows = collect_row_json(
                &self.ctx,
                Arc::clone(&hot_snapshot),
                entity_type,
                spill_enabled,
                result.count,
            )
            .await
            .unwrap_or_default();
            return MaterializedRowSource::Inline(rows);
        }

        crate::graph_cache_metrics::record_graph_surface_graph_backed(result.count);
        MaterializedRowSource::GraphBacked {
            entity_type: entity_type.to_string(),
            logical_count: result.count,
            hot_snapshot,
        }
    }

    /// CEP-5: parent entities for relation materialize — uses hot cache + spill when
    /// `result.entities` is empty but `result.count > 0` (GraphBacked surface).
    pub(crate) async fn resolve_source_parents(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> Vec<CachedEntity> {
        self.resolve_source_parents_with_identities(entity_type, result, &[])
            .await
    }

    /// CEP-5 / project-then-relate: prefer session-graph parents keyed by [`RowIdentity`]
    /// (and graph upgrades for synthetic/projected `result.entities`).
    pub(crate) async fn resolve_source_parents_with_identities(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
        row_identities: &[Option<plasm_core::RowIdentity>],
    ) -> Vec<CachedEntity> {
        let identity_bound = row_identities.iter().any(|opt| opt.is_some());
        let row_count = result.entities.len().max(row_identities.len());
        if row_count > 0 || identity_bound {
            let guard = self.ctx.es.lock_graph_cache().await;
            let mat = guard.materialization();
            let mut out = Vec::with_capacity(row_count);
            for idx in 0..row_count {
                let identity_row = row_identities.get(idx).and_then(|opt| opt.as_ref());
                let graph_parent = identity_row
                    .and_then(|id| mat.get(&id.reference))
                    .or_else(|| result.entities.get(idx).and_then(|e| mat.get(&e.reference)));
                if let Some(parent) = graph_parent {
                    out.push(parent.clone());
                } else if identity_row.is_none() {
                    if let Some(fallback) = result.entities.get(idx) {
                        out.push(fallback.clone());
                    }
                }
            }
            if !out.is_empty() || identity_bound {
                return out;
            }
        }
        if !result.entities.is_empty() {
            return result.entities.clone();
        }
        if result.count == 0 {
            return Vec::new();
        }
        let hot_snapshot = self.snapshot_hot_locked(entity_type).await;
        collect_entities(
            &self.ctx,
            hot_snapshot,
            entity_type,
            self.ctx.spill_enabled(),
            result.count,
        )
        .await
        .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) async fn materialize_entities_for_result(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> Vec<CachedEntity> {
        self.resolve_source_parents(entity_type, result).await
    }

    pub(crate) async fn rehydrate_rows(
        &self,
        hot_snapshot: Arc<[CachedEntity]>,
        entity_type: &str,
        logical_count: usize,
    ) -> Result<Vec<serde_json::Value>, String> {
        collect_row_json(
            &self.ctx,
            hot_snapshot,
            entity_type,
            self.ctx.spill_enabled(),
            logical_count,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn rehydrate_rows_locked(
        &self,
        entity_type: &str,
        logical_count: usize,
    ) -> Result<Vec<serde_json::Value>, String> {
        let hot = self.snapshot_hot_locked(entity_type).await;
        self.rehydrate_rows(hot, entity_type, logical_count).await
    }

    pub(crate) async fn stream_entity_rows<F>(
        &self,
        hot_snapshot: Arc<[CachedEntity]>,
        entity_type: &str,
        on_row: F,
    ) -> Result<(), String>
    where
        F: FnMut(&serde_json::Value) -> bool,
    {
        stream_rows(
            &self.ctx,
            hot_snapshot,
            entity_type,
            self.ctx.spill_enabled(),
            on_row,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn stream_entity_rows_locked<F>(
        &self,
        entity_type: &str,
        on_row: F,
    ) -> Result<(), String>
    where
        F: FnMut(&serde_json::Value) -> bool,
    {
        let hot = self.snapshot_hot_locked(entity_type).await;
        self.stream_entity_rows(hot, entity_type, on_row).await
    }

    pub(crate) async fn resolve_row_source_rows(
        &self,
        row_source: &MaterializedRowSource,
        max_rows: Option<usize>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let mut rows = match row_source {
            MaterializedRowSource::Inline(rows) => rows.clone(),
            MaterializedRowSource::GraphBacked {
                entity_type,
                logical_count,
                hot_snapshot,
            } => {
                self.rehydrate_rows(Arc::clone(hot_snapshot), entity_type, *logical_count)
                    .await?
            }
        };
        if let Some(n) = max_rows {
            rows.truncate(n);
        }
        Ok(rows)
    }
}

impl GraphSurfaceRehydrator<'_> {
    /// Plan + apply spill sync for a graph-backed result (`count > 0`, empty `entities`).
    pub(crate) async fn sync_result_from_materialization(
        hot: &SessionMaterialization,
        es: &ExecuteSession,
        st: &PlasmHostState,
        session_id: &str,
        entity_type: &str,
        cgs: &CGS,
        result: &mut ExecutionResult,
    ) {
        let Some(plan) = GraphSurfaceRehydrator::plan_spill_sync(hot, st, entity_type, result)
        else {
            return;
        };
        GraphSurfaceRehydrator::new(es, st, session_id, cgs)
            .apply_spill_sync(plan, result)
            .await;
    }
}
