//! Host [`GraphPageSpill`] backed by [`SessionGraphPersistence`] + unified [`SessionCore`] delta seq.

use std::sync::Arc;

use async_trait::async_trait;
use plasm_runtime::{
    CachedEntity, GraphHotCacheBounds, GraphPageDelta, GraphPageSpill, RuntimeError,
};

use crate::execute_session::SessionCore;
use crate::session_graph_persistence::SessionGraphPersistence;

pub fn graph_page_spill_for_execute(
    persistence: Option<&Arc<SessionGraphPersistence>>,
    core: Arc<SessionCore>,
    prompt_hash: &str,
    session_id: &str,
) -> Option<plasm_runtime::GraphPageSpillHandle> {
    let persistence = persistence.cloned()?;
    Some(Arc::new(AgentGraphPageSpill {
        persistence,
        core,
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
        hot_bounds: GraphHotCacheBounds::with_persistence_default(),
    }))
}

struct AgentGraphPageSpill {
    persistence: Arc<SessionGraphPersistence>,
    core: Arc<SessionCore>,
    prompt_hash: String,
    session_id: String,
    hot_bounds: GraphHotCacheBounds,
}

#[async_trait]
impl GraphPageSpill for AgentGraphPageSpill {
    async fn append_page(
        &self,
        page_index: usize,
        entities: &plasm_core::collection_codec::SharedRows<CachedEntity>,
    ) -> Result<(), RuntimeError> {
        if entities.is_empty() {
            return Ok(());
        }
        let entity_type = entities[0].reference.entity_type.to_string();
        let seq = self.core.alloc_delta_seq().await.0;
        let started = std::time::Instant::now();
        match self
            .persistence
            .append_graph_page(
                self.prompt_hash.as_str(),
                self.session_id.as_str(),
                seq,
                page_index,
                entity_type.as_str(),
                entities,
            )
            .await
        {
            Ok(()) => {
                crate::graph_cache_metrics::record_graph_delta_page_append(
                    entities.len(),
                    started.elapsed(),
                );
                Ok(())
            }
            Err(e) => {
                crate::graph_cache_metrics::record_graph_delta_page_append_error();
                Err(RuntimeError::CacheSource(Box::new(e)))
            }
        }
    }

    async fn graph_pages(&self) -> Result<Vec<GraphPageDelta>, RuntimeError> {
        self.persistence
            .read_graph_pages(self.prompt_hash.as_str(), self.session_id.as_str())
            .await
            .map_err(|error| RuntimeError::CacheSource(Box::new(error)))
    }

    fn hot_bounds(&self) -> GraphHotCacheBounds {
        self.hot_bounds
    }

    fn session_key(&self) -> (&str, &str) {
        (self.prompt_hash.as_str(), self.session_id.as_str())
    }
}

impl SessionGraphPersistence {
    #[allow(clippy::too_many_arguments)]
    pub async fn append_graph_page(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
        page_index: usize,
        entity_type: &str,
        entities: &plasm_core::collection_codec::SharedRows<CachedEntity>,
    ) -> Result<(), crate::session_graph_persistence::SessionGraphPersistenceError> {
        #[derive(serde::Serialize)]
        struct Page<'a> {
            kind: &'static str,
            schema_version: u32,
            entity_type: &'a str,
            page_index: usize,
            entities: &'a plasm_core::collection_codec::SharedRows<CachedEntity>,
        }
        let body = Page {
            kind: "graph_page",
            schema_version: crate::session_graph_persistence::GRAPH_PAGE_DELTA_SCHEMA_VERSION,
            entity_type,
            page_index,
            entities,
        };
        let payload = crate::run_artifacts::ArtifactPayload {
            metadata: crate::run_artifacts::ArtifactPayloadMetadata {
                content_type: "application/json".into(),
                content_encoding: None,
                schema_version: crate::run_artifacts::RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION,
                producer: crate::session_graph_persistence::GRAPH_PAGE_DELTA_PRODUCER.into(),
            },
            bytes: axum::body::Bytes::from(serde_json::to_vec(&body)?),
        };
        self.append_delta(prompt_hash, session_id, seq, &payload)
            .await
    }
}
