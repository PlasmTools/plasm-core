//! Recorded graph-surface rehydrate API (plan / apply / materialize).
//!
//! **CEP-4:** spill/rehydrate I/O runs without the session graph mutex held.
//! **CEP-5:** [`Self::resolve_source_parents`] keeps parent entities aligned with
//! [`MaterializedRowSource`] when [`ExecutionResult::entities`] is empty (GraphBacked).

use std::sync::Arc;

use plasm_core::CGS;
use plasm_runtime::{CachedEntity, ExecutionResult, MaterializedRowSource, SessionMaterialization};

use crate::execute_session::ExecuteSession;
use crate::server_state::PlasmHostState;

use super::ctx::GraphSurfaceWalkCtx;
use super::walk::snapshot_hot_entities;

/// Shared hot-cache snapshot for recorded rehydration after releasing the graph lock.
pub(crate) struct GraphSpillSyncPlan {
    pub hot_snapshot: Arc<[CachedEntity]>,
    pub entity_type: String,
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
        if !result.collection.is_graph_backed() || result.count() == 0 {
            return None;
        }
        Some(GraphSpillSyncPlan {
            hot_snapshot: snapshot_hot_entities(hot, entity_type),
            entity_type: entity_type.to_string(),
            spill_enabled: st.session_graph_persistence.is_some(),
        })
    }

    /// Apply spill rehydrate without holding the graph mutex (may read object store).
    pub(crate) async fn apply_spill_sync(
        &self,
        plan: GraphSpillSyncPlan,
        result: &mut ExecutionResult,
    ) -> Result<(), plasm_runtime::RuntimeError> {
        let rows = super::walk::collect_recorded_entities(
            &self.ctx,
            plan.hot_snapshot.into(),
            &plan.entity_type,
            plan.spill_enabled,
            result.collection.membership(),
        )
        .await
        .map_err(|error| plasm_runtime::RuntimeError::CacheSource(Box::new(error)))?;
        result.collection = result.collection.with_materialization(rows)?;
        Ok(())
    }

    pub(crate) async fn materialize_surface_rows(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> MaterializedRowSource {
        if !result.collection.is_graph_backed() {
            let guard = self.ctx.es.lock_graph_cache().await;
            let mat = guard.materialization();
            return MaterializedRowSource::Inline(
                result
                    .entities()
                    .iter()
                    .map(|e| {
                        super::relation_embed::wire_row_with_from_parent_embeds(
                            e,
                            self.ctx.cgs,
                            mat,
                        )
                    })
                    .collect(),
            );
        }
        if result.count() == 0 {
            return MaterializedRowSource::Inline(Vec::new());
        }

        let hot_snapshot = self.snapshot_hot_locked(entity_type).await;

        crate::graph_cache_metrics::record_graph_surface_graph_backed(result.count());
        MaterializedRowSource::GraphBacked {
            entity_type: entity_type.to_string(),
            membership: result.collection.membership().clone(),
            hot_snapshot,
        }
    }

    /// CEP-5: parent entities for relation materialize — uses hot cache + spill when
    /// `result.entities` is empty but `result.count > 0` (GraphBacked surface).
    pub(crate) async fn resolve_source_parents(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> Result<
        plasm_core::collection_codec::SharedRows<CachedEntity>,
        super::walk::GraphRehydrateError,
    > {
        self.resolve_source_parents_with_identities(entity_type, result, &[])
            .await
    }

    pub(crate) async fn resolve_source_parents_with_identities(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
        row_identities: &[Option<plasm_core::RowIdentity>],
    ) -> Result<
        plasm_core::collection_codec::SharedRows<CachedEntity>,
        super::walk::GraphRehydrateError,
    > {
        use plasm_core::collection_codec::{CollectionCodec, RecordingCodec, Transform};
        if !row_identities.is_empty() && row_identities.len() != result.count() {
            return Err(super::walk::GraphRehydrateError::RowIdentityCountMismatch);
        }
        let references: Vec<_> = result
            .collection
            .membership()
            .observed()
            .iter()
            .enumerate()
            .map(|(i, reference)| {
                row_identities
                    .get(i)
                    .and_then(Option::as_ref)
                    .map_or_else(|| reference.clone(), |row| row.reference.clone())
            })
            .collect();
        let membership = RecordingCodec::new().derive(
            result
                .collection
                .membership()
                .identity()
                .derived(&"canonical_identity_projection")?,
            &[result.collection.membership()],
            Transform::Map {
                rows: references.into(),
            },
        )?;
        let hot = self.snapshot_hot_locked(entity_type).await;
        // An identity-preserving projection names its canonical graph row, but
        // its projected payload cannot replace that row. Only unprojected source
        // observations may supply resident payloads absent from the graph.
        let existing: std::collections::HashSet<_> = hot.iter().map(|row| &row.reference).collect();
        let positions = result.entities().iter().enumerate().filter_map(|(i, row)| {
            (row_identities.get(i).and_then(Option::as_ref).is_none()
                && !existing.contains(&row.reference))
            .then_some(i)
        });
        let retained = result.entities().select(positions)?;
        let available = plasm_core::collection_codec::SharedRows::concat([&hot.into(), &retained]);
        let rows = super::walk::collect_recorded_entities(
            &self.ctx,
            available,
            entity_type,
            self.ctx.spill_enabled(),
            &membership,
        )
        .await?;
        Ok(rows)
    }

    #[cfg(test)]
    pub(crate) async fn materialize_entities_for_result(
        &self,
        entity_type: &str,
        result: &ExecutionResult,
    ) -> plasm_core::collection_codec::SharedRows<CachedEntity> {
        self.resolve_source_parents(entity_type, result)
            .await
            .unwrap()
    }

    #[cfg(test)]
    pub(crate) async fn rehydrate_rows_locked(
        &self,
        entity_type: &str,
        membership: &plasm_core::collection_codec::RecordedCollection<plasm_core::Ref>,
    ) -> Result<Vec<plasm_core::ValueRow>, super::walk::GraphRehydrateError> {
        let hot = self.snapshot_hot_locked(entity_type).await;
        let rows = super::walk::collect_recorded_entities(
            &self.ctx,
            hot.into(),
            entity_type,
            self.ctx.spill_enabled(),
            membership,
        )
        .await?;
        Ok(rows
            .iter()
            .map(|row| plasm_runtime::entity_to_row_values(row, Some(self.ctx.cgs)))
            .collect())
    }

    pub(crate) async fn resolve_row_source_rows(
        &self,
        row_source: &MaterializedRowSource,
        max_rows: Option<usize>,
    ) -> Result<Vec<plasm_core::ValueRow>, super::walk::GraphRehydrateError> {
        let mut rows = match row_source {
            MaterializedRowSource::Inline(rows) => rows.clone(),
            MaterializedRowSource::GraphBacked {
                entity_type,
                membership,
                hot_snapshot,
            } => super::walk::collect_recorded_entities(
                &self.ctx,
                Arc::clone(hot_snapshot).into(),
                entity_type,
                self.ctx.spill_enabled(),
                membership,
            )
            .await?
            .iter()
            .map(|row| plasm_runtime::entity_to_row_values(row, Some(self.ctx.cgs)))
            .collect(),
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
    ) -> Result<(), plasm_runtime::RuntimeError> {
        let Some(plan) = GraphSurfaceRehydrator::plan_spill_sync(hot, st, entity_type, result)
        else {
            return Ok(());
        };
        GraphSurfaceRehydrator::new(es, st, session_id, cgs)
            .apply_spill_sync(plan, result)
            .await
    }
}
