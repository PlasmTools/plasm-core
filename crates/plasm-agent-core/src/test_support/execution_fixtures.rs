//! Synthetic execution results for MCP publish / output tests.

use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::{EntityKey, Ref, Value};
use plasm_runtime::{
    CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource, ExecutionStats,
    OperationLedger, ResultCoverage,
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
                    key: EntityKey::Simple(plasm_core::IdentitySlot::lit(format!("m{i}"))),
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
            collection: plasm_runtime::execution::ExecutionCollection::observe(
                plasm_core::collection_codec::CollectionIdentity {
                    catalog: [1; 32],
                    expression: [2; 32],
                    epoch: 0,
                },
                entities,
                plasm_core::collection_codec::Observation::UnprovenPage,
            )
            .unwrap(),
            has_more: paging_handle.is_some(),
            pagination_resume: None,
            paging_handle,
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations: OperationLedger::empty(),
        }),
        artifact,
    }
}

/// Construct an explicit producer observation for tests; production accepts no coverage setter.
pub fn collection(
    rows: Vec<CachedEntity>,
    coverage: ResultCoverage,
) -> plasm_runtime::execution::ExecutionCollection {
    use plasm_core::collection_codec::{CollectionIdentity, Observation};
    let observation = match coverage {
        ResultCoverage::Complete => Observation::ExactOutput {
            decoded: rows.len(),
        },
        ResultCoverage::Partial => Observation::MoreAvailable,
        ResultCoverage::Unknown => Observation::UnprovenPage,
    };
    plasm_runtime::execution::ExecutionCollection::observe(
        CollectionIdentity::for_untyped_observation(&"execution_fixture").unwrap(),
        rows,
        observation,
    )
    .unwrap()
}

#[cfg(test)]
pub fn checkpoint(
    rows: Vec<CachedEntity>,
    coverage: ResultCoverage,
) -> plasm_core::collection_codec::CollectionCheckpoint {
    plasm_core::collection_codec::CollectionCheckpoint::capture(
        &plasm_core::collection_codec::RecordingCodec::new(),
        collection(rows, coverage).membership(),
    )
    .unwrap()
}
