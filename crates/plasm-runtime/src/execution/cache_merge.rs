//! Merge decoded query results into the session graph.

use super::*;
use crate::materialization::{CacheTelemetry, SessionMaterialization};
use crate::{CachedEntity, EntityCompleteness, RuntimeError};

pub(crate) struct DecodedQueryBatch {
    pub entities: Vec<CachedEntity>,
    pub stats: ExecutionStats,
}

pub(crate) fn query_result_merge_cache(
    decoded_entities: Vec<plasm_compile::DecodedEntity>,
    completeness: impl Fn(&plasm_compile::DecodedEntity) -> EntityCompleteness,
    _source: ExecutionSource,
    mat: &mut SessionMaterialization,
    network_requests: usize,
) -> Result<DecodedQueryBatch, RuntimeError> {
    let timestamp = current_timestamp();
    let mut cached_entities = Vec::new();
    for decoded in decoded_entities {
        let completeness = completeness(&decoded);
        cached_entities.push(embed_cache::cache_decoded_entity_tree(
            mat,
            decoded,
            timestamp,
            completeness,
        )?);
    }
    let count = cached_entities.len();
    mat.merge(cached_entities.clone())?;
    let mut stats = ExecutionStats::from_telemetry(CacheTelemetry::default(), network_requests);
    stats.record_rows_materialized(count);
    Ok(DecodedQueryBatch {
        entities: cached_entities,
        stats,
    })
}
