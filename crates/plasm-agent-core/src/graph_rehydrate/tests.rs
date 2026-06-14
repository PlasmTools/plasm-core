use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::test_support::graph_fixtures::{
    berry_entity, load_pokeapi_mini_cgs, test_execute_session, SpillHostFixture,
};

const SID: &str = "graph_rehydrate_test_sid";

static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) struct GraphRehydrateFixture {
    pub cgs: Arc<plasm_core::CGS>,
    pub es: crate::execute_session::ExecuteSession,
    pub host: SpillHostFixture,
    pub prompt_hash: String,
}

impl GraphRehydrateFixture {
    pub fn new() -> Self {
        let prompt_hash = format!(
            "graph_rehydrate_test_ph_{}",
            FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let cgs = load_pokeapi_mini_cgs();
        let es = test_execute_session(cgs.clone(), &prompt_hash);
        let host = SpillHostFixture::new();
        Self {
            cgs,
            es,
            host,
            prompt_hash,
        }
    }

    pub async fn insert_hot(&self, entities: &[plasm_runtime::CachedEntity]) {
        let mut cache = self.es.lock_graph_cache().await;
        for entity in entities {
            cache.insert(entity.clone()).expect("insert hot");
        }
    }

    pub async fn spill(&self, pages: &[Vec<plasm_runtime::CachedEntity>]) {
        let core = crate::execute_session::SessionCore::new();
        for (page_index, page_entities) in pages.iter().enumerate() {
            let seq = core.alloc_delta_seq().await.0;
            self.host
                .persistence
                .append_graph_page(
                    self.prompt_hash.as_str(),
                    SID,
                    seq,
                    page_index,
                    "Berry",
                    page_entities,
                    None,
                )
                .await
                .expect("append spill page");
        }
    }
}

async fn spill_refless_rows(fx: &GraphRehydrateFixture, rows: Vec<serde_json::Value>) {
    let core = crate::execute_session::SessionCore::new();
    let seq = core.alloc_delta_seq().await.0;
    let body = serde_json::json!({
        "kind": "graph_page",
        "schema_version": 2,
        "entity_type": "Berry",
        "page_index": 0,
        "entities": rows,
    });
    let payload = crate::run_artifacts::ArtifactPayload {
        metadata: crate::run_artifacts::ArtifactPayloadMetadata {
            content_type: "application/json".into(),
            content_encoding: None,
            schema_version: 2,
            producer: "plasm.graph_rehydrate_test".into(),
        },
        bytes: axum::body::Bytes::from(serde_json::to_vec(&body).expect("json")),
    };
    fx.host
        .persistence
        .append_delta(fx.prompt_hash.as_str(), SID, seq, &payload)
        .await
        .expect("append refless page");
}

#[tokio::test]
async fn stream_entity_rows_dedupes_hot_and_overlapping_spill_pages() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("cheri"), berry_entity("chesto")])
        .await;
    fx.spill(&[
        vec![
            berry_entity("cheri"),
            berry_entity("chesto"),
            berry_entity("pecha"),
        ],
        vec![
            berry_entity("cheri"),
            berry_entity("pecha"),
            berry_entity("rawst"),
        ],
    ])
    .await;

    let mut streamed = Vec::new();
    super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
        .stream_entity_rows_locked("Berry", |row| {
            streamed.push(row.clone());
            false
        })
        .await
        .expect("stream");

    let unique_refs: BTreeSet<String> = streamed
        .iter()
        .filter_map(|r| r.get("_ref").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert_eq!(
        streamed.len(),
        unique_refs.len(),
        "stream must not duplicate _ref rows: {streamed:?}"
    );
    assert_eq!(streamed.len(), 4, "cheri, chesto, pecha, rawst");
}

#[tokio::test]
async fn rehydrate_and_stream_yield_same_deduped_count() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("cheri")]).await;
    fx.spill(&[vec![berry_entity("cheri"), berry_entity("pecha")]])
        .await;

    let full =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", 10)
            .await
            .expect("rehydrate");
    let mut streamed = Vec::new();
    super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
        .stream_entity_rows_locked("Berry", |row| {
            streamed.push(row.clone());
            false
        })
        .await
        .expect("stream");
    assert_eq!(full.len(), streamed.len());
    assert_eq!(full.len(), 2);
}

#[tokio::test]
async fn stream_dedupes_refless_spill_rows_by_id_field() {
    let fx = GraphRehydrateFixture::new();
    spill_refless_rows(
        &fx,
        vec![
            serde_json::json!({"name": "cheri"}),
            serde_json::json!({"name": "cheri"}),
            serde_json::json!({"name": "pecha"}),
        ],
    )
    .await;

    let mut streamed = Vec::new();
    super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
        .stream_entity_rows_locked("Berry", |row| {
            streamed.push(row.clone());
            false
        })
        .await
        .expect("stream");

    assert_eq!(
        streamed.len(),
        2,
        "refless duplicates collapse by name/id_field"
    );
}

#[tokio::test]
async fn materialized_entities_use_walker_when_persistence_exists() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("cheri")]).await;
    fx.spill(&[vec![berry_entity("cheri"), berry_entity("pecha")]])
        .await;

    let result = plasm_runtime::ExecutionResult {
        entities: Vec::new(),
        count: 10,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: plasm_runtime::ExecutionSource::Live,
        stats: plasm_runtime::ExecutionStats::default(),
        request_fingerprints: Vec::new(),
    };
    let direct =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", 10)
            .await
            .expect("rehydrate rows");
    assert_eq!(direct.len(), 2);

    let entities =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .materialize_entities_for_result("Berry", &result)
            .await;

    assert_eq!(entities.len(), 2);
    let names: BTreeSet<String> = entities
        .iter()
        .map(|e| e.reference.primary_slot_str().to_string())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains("cheri"));
    assert!(names.contains("pecha"));
}
