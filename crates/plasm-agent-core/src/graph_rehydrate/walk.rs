//! Resolve recorded occurrences from shared hot-cache and spill batches.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use plasm_runtime::CachedEntity;

use super::ctx::GraphSurfaceWalkCtx;

/// Copy hot-cache entities for `entity_type` (bounded by hot trim).
pub(crate) fn snapshot_hot_entities(
    hot: &plasm_runtime::SessionMaterialization,
    entity_type: &str,
) -> Arc<[CachedEntity]> {
    Arc::from(
        hot.get_entities_by_type(entity_type)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
    )
}

/// Fetch exactly the recorded identities, then replay occurrence order. Cache
/// iteration order, unrelated rows and duplicate identities cannot change membership.
pub(crate) async fn collect_recorded_entities(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_snapshot: plasm_core::collection_codec::SharedRows<CachedEntity>,
    entity_type: &str,
    spill_enabled: bool,
    membership: &plasm_core::collection_codec::RecordedCollection<plasm_core::Ref>,
) -> Result<plasm_core::collection_codec::SharedRows<CachedEntity>, String> {
    use tracing::Instrument;
    collect_recorded_entities_inner(ctx, hot_snapshot, entity_type, spill_enabled, membership)
        .instrument(crate::spans::execute_graph_rehydrate(
            "recorded",
            membership.observed().len(),
        ))
        .await
}

async fn collect_recorded_entities_inner(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_snapshot: plasm_core::collection_codec::SharedRows<CachedEntity>,
    entity_type: &str,
    spill_enabled: bool,
    membership: &plasm_core::collection_codec::RecordedCollection<plasm_core::Ref>,
) -> Result<plasm_core::collection_codec::SharedRows<CachedEntity>, String> {
    use plasm_core::collection_codec::SharedRows;
    let started = Instant::now();
    let mut pages_read = 0;
    let mut pending: HashSet<_> = membership.observed().iter().collect();
    if pending.is_empty() {
        return Ok(Vec::new().into());
    }
    let selected: Vec<_> = hot_snapshot
        .iter()
        .enumerate()
        .filter_map(|(i, row)| pending.remove(&row.reference).then_some(i))
        .collect();
    let mut batches = vec![hot_snapshot.select(selected).map_err(|e| e.to_string())?];
    if !pending.is_empty() && spill_enabled {
        if let Some(persistence) = ctx.st.session_graph_persistence.as_ref() {
            pages_read = persistence
                .visit_graph_pages_in_seq_order(
                    ctx.es.prompt_hash.as_str(),
                    ctx.session_id,
                    |page| {
                        if !page.entity_type.is_empty() && page.entity_type != entity_type {
                            return Ok(ControlFlow::Continue(()));
                        }
                        let rows = SharedRows::from(page.entities);
                        let selected: Vec<_> = rows
                            .iter()
                            .enumerate()
                            .filter_map(|(i, row)| pending.remove(&row.reference).then_some(i))
                            .collect();
                        batches.push(rows.select(selected).map_err(|e| e.to_string())?);
                        Ok(if pending.is_empty() {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        })
                    },
                )
                .await?;
        }
    }
    if !pending.is_empty() {
        return Err(format!(
            "{} recorded identities unavailable during graph rehydration",
            pending.len()
        ));
    }
    crate::graph_cache_metrics::record_graph_rehydrate(
        "recorded",
        membership.observed().len(),
        pages_read,
        started.elapsed(),
    );
    let rows = SharedRows::concat(batches.iter());
    let positions: std::collections::HashMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| (&row.reference, i))
        .collect();
    rows.select(
        membership
            .observed()
            .iter()
            .map(|reference| positions[reference]),
    )
    .map_err(|e| e.to_string())
}
