//! Merge decoded query results into the session graph.

use super::*;
use crate::materialization::{CacheTelemetry, SessionMaterialization};
use crate::{CachedEntity, EntityCompleteness, RuntimeError};

pub(crate) fn query_result_merge_cache(
    decoded_entities: Vec<plasm_compile::DecodedEntity>,
    completeness: EntityCompleteness,
    source: ExecutionSource,
    mat: &mut SessionMaterialization,
    network_requests: usize,
) -> Result<ExecutionResult, RuntimeError> {
    let timestamp = current_timestamp();
    let mut cached_entities = Vec::new();
    for decoded in decoded_entities {
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
    Ok(ExecutionResult {
        entities: cached_entities,
        count,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints: Vec::new(),
        operations: OperationLedger::empty(),
    })
}
