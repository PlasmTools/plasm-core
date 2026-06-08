//! Rehydrate graph-backed rows from hot cache + spilled pages for plan compute.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use plasm_core::CGS;
use plasm_runtime::{entity_to_row_json, CachedEntity, MaterializedRowSource};

use crate::execute_session::ExecuteSession;
use crate::server_state::PlasmHostState;
use crate::spans;

pub(crate) async fn materialize_surface_rows(
    es: &ExecuteSession,
    st: &PlasmHostState,
    cgs: &CGS,
    entity_type: &str,
    result: &plasm_runtime::ExecutionResult,
) -> MaterializedRowSource {
    if !result.entities.is_empty() {
        return MaterializedRowSource::Inline(
            result
                .entities
                .iter()
                .map(|e| entity_to_row_json(e, Some(cgs)))
                .collect(),
        );
    }
    if result.count == 0 {
        return MaterializedRowSource::Inline(Vec::new());
    }
    let graph = es.graph_cache.lock().await;
    let hot_len = graph.get_entities_by_type(entity_type).len();
    drop(graph);
    if hot_len >= result.count || st.session_graph_persistence.is_none() {
        let graph = es.graph_cache.lock().await;
        return MaterializedRowSource::Inline(
            graph
                .get_entities_by_type(entity_type)
                .into_iter()
                .map(|e| entity_to_row_json(e, Some(cgs)))
                .collect(),
        );
    }
    crate::graph_cache_metrics::record_graph_surface_graph_backed(result.count);
    MaterializedRowSource::GraphBacked {
        entity_type: entity_type.to_string(),
        logical_count: result.count,
    }
}

pub(crate) async fn materialized_entities_for_surface(
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    cgs: &CGS,
    entity_type: &str,
    result: &plasm_runtime::ExecutionResult,
) -> Vec<CachedEntity> {
    if !result.entities.is_empty() {
        return result.entities.clone();
    }
    if result.count == 0 {
        return Vec::new();
    }
    let graph = es.graph_cache.lock().await;
    let hot_len = graph.get_entities_by_type(entity_type).len();
    drop(graph);
    if hot_len >= result.count || st.session_graph_persistence.is_none() {
        let graph = es.graph_cache.lock().await;
        return graph
            .get_entities_by_type(entity_type)
            .into_iter()
            .cloned()
            .collect();
    }
    rehydrate_entities(es, st, session_id, entity_type, result.count, cgs)
        .await
        .unwrap_or_default()
}

pub(crate) async fn rehydrate_rows(
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    entity_type: &str,
    logical_count: usize,
    cgs: &CGS,
) -> Result<Vec<serde_json::Value>, String> {
    let entities =
        rehydrate_entities(es, st, session_id, entity_type, logical_count, cgs).await?;
    Ok(entities
        .iter()
        .map(|e| entity_to_row_json(e, Some(cgs)))
        .collect())
}

pub(crate) async fn stream_entity_rows<F>(
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    entity_type: &str,
    cgs: &CGS,
    mut on_row: F,
) -> Result<(), String>
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let span = spans::execute_graph_rehydrate("stream", 0);
    let _guard = span.enter();
    let started = Instant::now();
    let mut rows_seen = 0usize;

    let graph = es.graph_cache.lock().await;
    for entity in graph.get_entities_by_type(entity_type) {
        let row = entity_to_row_json(entity, Some(cgs));
        rows_seen += 1;
        if on_row(&row) {
            crate::graph_cache_metrics::record_graph_rehydrate("stream", rows_seen, 0, started.elapsed());
            return Ok(());
        }
    }
    drop(graph);

    let persistence = st
        .session_graph_persistence
        .as_ref()
        .ok_or_else(|| "graph rehydrate requires PLASM_GRAPH_CACHE_URL".to_string())?;
    let pages = persistence
        .read_graph_pages(es.prompt_hash.as_str(), session_id)
        .await?;
    let pages_read = pages.len();
    let hot_refs: HashSet<String> = {
        let graph = es.graph_cache.lock().await;
        graph
            .get_entities_by_type(entity_type)
            .iter()
            .map(|e| e.reference.to_string())
            .collect()
    };
    for page in pages {
        if !page.entity_type.is_empty() && page.entity_type != entity_type {
            continue;
        }
        for row in &page.entities {
            if let Some(r) = row.get("_ref").and_then(|v| v.as_str()) {
                if hot_refs.contains(r) {
                    continue;
                }
            }
            rows_seen += 1;
            if on_row(row) {
                crate::graph_cache_metrics::record_graph_rehydrate(
                    "stream",
                    rows_seen,
                    pages_read,
                    started.elapsed(),
                );
                return Ok(());
            }
        }
    }
    crate::graph_cache_metrics::record_graph_rehydrate(
        "stream",
        rows_seen,
        pages_read,
        started.elapsed(),
    );
    Ok(())
}

async fn rehydrate_entities(
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    entity_type: &str,
    logical_count: usize,
    cgs: &CGS,
) -> Result<Vec<CachedEntity>, String> {
    let span = spans::execute_graph_rehydrate("full", logical_count);
    let _guard = span.enter();
    let started = Instant::now();

    let persistence = st.session_graph_persistence.as_ref().ok_or_else(|| {
        "graph rehydrate requires PLASM_GRAPH_CACHE_URL".to_string()
    })?;
    let pages = persistence
        .read_graph_pages(es.prompt_hash.as_str(), session_id)
        .await?;
    let pages_read = pages.len();

    let mut by_ref: BTreeMap<String, CachedEntity> = BTreeMap::new();
    for page in pages {
        if !page.entity_type.is_empty() && page.entity_type != entity_type {
            continue;
        }
        for row in &page.entities {
            if let Ok(entity) = CachedEntity::from_row_json(entity_type, row, cgs) {
                by_ref.insert(entity.reference.to_string(), entity);
            }
        }
    }

    let graph = es.graph_cache.lock().await;
    for entity in graph.get_entities_by_type(entity_type) {
        by_ref.insert(entity.reference.to_string(), entity.clone());
    }
    drop(graph);

    let mut out: Vec<CachedEntity> = by_ref.into_values().collect();
    if out.len() > logical_count {
        out.truncate(logical_count);
    }
    crate::graph_cache_metrics::record_graph_rehydrate(
        "full",
        out.len(),
        pages_read,
        started.elapsed(),
    );
    Ok(out)
}

pub(crate) async fn resolve_row_source_rows(
    row_source: &MaterializedRowSource,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    cgs: &CGS,
) -> Result<Vec<serde_json::Value>, String> {
    match row_source {
        MaterializedRowSource::Inline(rows) => Ok(rows.clone()),
        MaterializedRowSource::GraphBacked {
            entity_type,
            logical_count,
        } => rehydrate_rows(es, st, session_id, entity_type, *logical_count, cgs).await,
    }
}
