//! Host [`GraphPageSpill`] backed by [`SessionGraphPersistence`] + unified [`SessionCore`] delta seq.

use std::sync::Arc;

use async_trait::async_trait;
use plasm_core::CGS;
use plasm_runtime::{
    entity_to_row_json, CachedEntity, GraphHotCacheBounds, GraphPageDelta, GraphPageSpill,
    RuntimeError,
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
        entities: &[CachedEntity],
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
                None,
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
                Err(RuntimeError::CacheError { message: e })
            }
        }
    }

    async fn graph_pages(&self) -> Result<Vec<GraphPageDelta>, RuntimeError> {
        self.persistence
            .read_graph_pages(self.prompt_hash.as_str(), self.session_id.as_str())
            .await
            .map_err(|e| RuntimeError::CacheError { message: e })
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
        entities: &[CachedEntity],
        cgs: Option<&CGS>,
    ) -> Result<(), String> {
        let rows: Vec<serde_json::Value> = entities
            .iter()
            .map(|e| entity_to_row_json(e, cgs))
            .collect();
        let body = serde_json::json!({
            "kind": "graph_page",
            "schema_version": 2,
            "entity_type": entity_type,
            "page_index": page_index,
            "entities": rows,
        });
        let payload = crate::run_artifacts::ArtifactPayload {
            metadata: crate::run_artifacts::ArtifactPayloadMetadata {
                content_type: "application/json".into(),
                content_encoding: None,
                schema_version: 2,
                producer: "plasm.graph_page_spill".into(),
            },
            bytes: axum::body::Bytes::from(serde_json::to_vec(&body).map_err(|e| e.to_string())?),
        };
        self.append_delta(prompt_hash, session_id, seq, &payload)
            .await
    }
}
