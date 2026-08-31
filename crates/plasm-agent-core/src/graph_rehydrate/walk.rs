//! Hot-cache + spill-page walker with dedup and early exit.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use plasm_core::CGS;
use plasm_runtime::{entity_to_row_json, CachedEntity};

use crate::spans;

use super::ctx::GraphSurfaceWalkCtx;

pub(crate) struct GraphSurfaceWalkStats {
    pub rows_yielded: usize,
    pub pages_read: usize,
}

/// Tracks entity identity already yielded across hot cache and spill pages.
#[derive(Default)]
struct SeenEntityRefs(HashSet<String>);

impl SeenEntityRefs {
    fn insert_unique(&mut self, key: &str) -> bool {
        if key.is_empty() {
            return true;
        }
        if self.0.contains(key) {
            return false;
        }
        self.0.insert(key.to_string());
        true
    }

    fn row_dedup_key(entity_type: &str, cgs: &CGS, row: &serde_json::Value) -> Option<String> {
        if let Some(r) = row.get("_ref").and_then(|v| v.as_str()) {
            if !r.is_empty() {
                return Some(r.to_string());
            }
        }
        let id_name = cgs.get_entity(entity_type)?.id_field.as_str();
        let id_val = row.get(id_name)?;
        let slot = match id_val {
            serde_json::Value::String(s) if !s.is_empty() => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => return None,
        };
        Some(format!("{entity_type}:{slot}"))
    }

    fn insert_row(&mut self, entity_type: &str, cgs: &CGS, row: &serde_json::Value) -> bool {
        match Self::row_dedup_key(entity_type, cgs, row) {
            Some(key) => self.insert_unique(&key),
            None => true,
        }
    }
}

/// Walk hot entities then spill pages; `on_row` returns `true` to stop early.
pub(crate) async fn walk_graph_surface<F>(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_entities: Arc<[CachedEntity]>,
    entity_type: &str,
    spill_enabled: bool,
    mut on_row: F,
) -> Result<GraphSurfaceWalkStats, String>
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let cgs = ctx.cgs;
    let mut seen = SeenEntityRefs::default();
    let mut rows_yielded = 0usize;

    for entity in hot_entities.iter() {
        if !seen.insert_unique(&entity.reference.to_string()) {
            continue;
        }
        rows_yielded += 1;
        let row = entity_to_row_json(entity, Some(cgs));
        if on_row(&row) {
            return Ok(GraphSurfaceWalkStats {
                rows_yielded,
                pages_read: 0,
            });
        }
    }

    if !spill_enabled {
        return Ok(GraphSurfaceWalkStats {
            rows_yielded,
            pages_read: 0,
        });
    }

    let Some(persistence) = ctx.st.session_graph_persistence.as_ref() else {
        return Ok(GraphSurfaceWalkStats {
            rows_yielded,
            pages_read: 0,
        });
    };

    let pages_read = persistence
        .visit_graph_pages_in_seq_order(ctx.es.prompt_hash.as_str(), ctx.session_id, |page| {
            if !page.entity_type.is_empty() && page.entity_type != entity_type {
                return Ok(ControlFlow::Continue(()));
            }
            for row in &page.entities {
                if !seen.insert_row(entity_type, cgs, row) {
                    continue;
                }
                rows_yielded += 1;
                if on_row(row) {
                    return Ok(ControlFlow::Break(()));
                }
            }
            Ok(ControlFlow::Continue(()))
        })
        .await?;

    Ok(GraphSurfaceWalkStats {
        rows_yielded,
        pages_read,
    })
}

#[cfg(test)]
pub(crate) async fn stream_rows<F>(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_snapshot: Arc<[CachedEntity]>,
    entity_type: &str,
    spill_enabled: bool,
    mut on_row: F,
) -> Result<(), String>
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let span = spans::execute_graph_rehydrate("stream", 0);
    let _guard = span.enter();
    let started = Instant::now();

    let stats = walk_graph_surface(ctx, hot_snapshot, entity_type, spill_enabled, |row| {
        on_row(row)
    })
    .await?;

    crate::graph_cache_metrics::record_graph_rehydrate(
        "stream",
        stats.rows_yielded,
        stats.pages_read,
        started.elapsed(),
    );
    Ok(())
}

pub(crate) async fn collect_entities(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_snapshot: Arc<[CachedEntity]>,
    entity_type: &str,
    spill_enabled: bool,
    logical_count: usize,
) -> Result<Vec<CachedEntity>, String> {
    let span = spans::execute_graph_rehydrate("full", logical_count);
    let _guard = span.enter();
    let started = Instant::now();
    let cgs = ctx.cgs;

    let mut out = Vec::new();
    let stats = walk_graph_surface(ctx, hot_snapshot, entity_type, spill_enabled, |row| {
        if let Ok(entity) = CachedEntity::from_row_json(entity_type, row, cgs) {
            out.push(entity);
            if out.len() >= logical_count {
                return true;
            }
        }
        false
    })
    .await?;

    out.truncate(logical_count);
    crate::graph_cache_metrics::record_graph_rehydrate(
        "full",
        stats.rows_yielded,
        stats.pages_read,
        started.elapsed(),
    );
    Ok(out)
}

pub(crate) async fn collect_row_json(
    ctx: &GraphSurfaceWalkCtx<'_>,
    hot_snapshot: Arc<[CachedEntity]>,
    entity_type: &str,
    spill_enabled: bool,
    logical_count: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let cgs = ctx.cgs;
    let entities =
        collect_entities(ctx, hot_snapshot, entity_type, spill_enabled, logical_count).await?;
    Ok(entities
        .iter()
        .map(|e| entity_to_row_json(e, Some(cgs)))
        .collect())
}

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
