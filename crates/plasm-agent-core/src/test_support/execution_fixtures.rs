//! Synthetic execution results for MCP publish / output tests.

use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::{EntityKey, Ref, Value};
use plasm_runtime::{
    CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource, ExecutionStats,
};

use crate::http_execute::PublishedResultStep;

pub fn synthetic_published_result_step(
    row_count: usize,
    artifact: Option<crate::run_artifacts::RunArtifactHandle>,
) -> PublishedResultStep {
    synthetic_published_result_step_with_paging(row_count, artifact, None)
}

pub fn synthetic_published_result_step_with_paging(
    row_count: usize,
    artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    paging_handle: Option<plasm_core::PagingHandle>,
) -> PublishedResultStep {
    let entities: Vec<CachedEntity> = (0..row_count)
        .map(|i| {
            let mut fields = IndexMap::new();
            fields.insert("id".into(), Value::String(format!("m{i}")));
            fields.insert("name".into(), Value::String(format!("move-{i}")));
            CachedEntity::from_decoded(
                Ref {
                    entity_type: "Move".into(),
                    key: EntityKey::Simple(format!("m{i}").into()),
                },
                fields,
                IndexMap::new(),
                0,
                EntityCompleteness::Complete,
            )
        })
        .collect();
    PublishedResultStep {
        name: Some("moves".into()),
        node_id: None,
        entry_id: Some("pokeapi".into()),
        entity: Some("Move".into()),
        cgs: None,
        display: "Move[id,name]".into(),
        projection: Some(vec!["id".into(), "name".into()]),
        result: Arc::new(ExecutionResult {
            count: row_count,
            entities,
            has_more: paging_handle.is_some(),
            pagination_resume: None,
            paging_handle,
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
        }),
        artifact,
    }
}
