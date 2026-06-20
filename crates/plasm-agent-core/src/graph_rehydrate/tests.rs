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

const PROJECT_THEN_RELATE_SID: &str = "project_then_relate_sid";

#[tokio::test]
async fn matrix_row_identity_upgrades_to_graph_parent() {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use plasm_core::{
        loader::load_schema_dir, IdEncoding, QualifiedEntityKey, Ref, RowIdentity, TypedFieldValue,
        Value,
    };
    use plasm_runtime::{CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource};

    use crate::test_support::graph_fixtures::{test_execute_session, SpillHostFixture};

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let sess = test_execute_session(cgs.clone(), "project_relate_matrix");
    let tag_ref = Ref::new("LangTag", "t1");
    let item_ref = Ref::new("LangItem", "i1");
    let parent = CachedEntity {
        reference: item_ref.clone(),
        fields: IndexMap::new(),
        relations: IndexMap::from([("tags".into(), vec![tag_ref])]),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
    };
    let projected = CachedEntity {
        reference: item_ref.clone(),
        fields: IndexMap::from([(
            "title".into(),
            TypedFieldValue::from(Value::String("Demo".into())),
        )]),
        relations: IndexMap::new(),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Summary,
    };
    {
        let mut guard = sess.lock_graph_cache().await;
        guard.insert(parent).expect("insert");
    }

    let row_identity = RowIdentity::new(
        QualifiedEntityKey::new("default", "LangItem"),
        item_ref,
        IndexMap::new(),
        IdEncoding::Simple,
    );
    let result = ExecutionResult {
        entities: vec![projected],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
    };
    let host = SpillHostFixture::new();
    let parents = super::GraphSurfaceRehydrator::new(
        &sess,
        host.st.as_ref(),
        PROJECT_THEN_RELATE_SID,
        cgs.as_ref(),
    )
    .resolve_source_parents_with_identities("LangItem", &result, &[Some(row_identity)])
    .await;
    assert_eq!(parents.len(), 1);
    assert_eq!(
        parents[0].relations.get("tags").map(|v| v.len()),
        Some(1),
        "projected row must upgrade to graph parent with tags relation refs"
    );
}

#[tokio::test]
async fn row_identity_graph_miss_omits_thin_projected_fallback() {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use plasm_core::{
        loader::load_schema_dir, IdEncoding, QualifiedEntityKey, Ref, RowIdentity, TypedFieldValue,
        Value,
    };
    use plasm_runtime::{CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource};

    use crate::test_support::graph_fixtures::{test_execute_session, SpillHostFixture};

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let sess = test_execute_session(cgs.clone(), "project_relate_miss");
    let item_ref = Ref::new("LangItem", "i1");
    let projected = CachedEntity {
        reference: item_ref.clone(),
        fields: IndexMap::from([(
            "title".into(),
            TypedFieldValue::from(Value::String("Demo".into())),
        )]),
        relations: IndexMap::new(),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Summary,
    };
    let row_identity = RowIdentity::new(
        QualifiedEntityKey::new("default", "LangItem"),
        item_ref,
        IndexMap::new(),
        IdEncoding::Simple,
    );
    let result = ExecutionResult {
        entities: vec![projected],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
    };
    let host = SpillHostFixture::new();
    let parents = super::GraphSurfaceRehydrator::new(
        &sess,
        host.st.as_ref(),
        PROJECT_THEN_RELATE_SID,
        cgs.as_ref(),
    )
    .resolve_source_parents_with_identities("LangItem", &result, &[Some(row_identity)])
    .await;
    assert!(
        parents.is_empty(),
        "identity-bound rows must not fall back to thin projected entities when graph parent is missing"
    );
}

#[tokio::test]
async fn pokeapi_type_pokemon_plan_prefers_graph_parent() {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use plasm_core::{loader::load_schema_dir, Ref, TypedFieldValue, Value};
    use plasm_runtime::{CachedEntity, EntityCompleteness};

    use crate::test_support::graph_fixtures::test_execute_session;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    if !dir.is_dir() {
        return;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("pokeapi"));
    let type_rel = cgs
        .get_entity("Type")
        .and_then(|e| e.relations.get("pokemon"))
        .and_then(|r| r.materialize.clone())
        .expect("Type.pokemon materialize");
    let sess = test_execute_session(cgs.clone(), "type_pokemon_plan");
    let pikachu = CachedEntity {
        reference: Ref::new("Pokemon", "pikachu"),
        fields: IndexMap::from([(
            "name".into(),
            TypedFieldValue::from(Value::String("pikachu".into())),
        )]),
        relations: IndexMap::new(),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
    };
    let electric = CachedEntity {
        reference: Ref::new("Type", "electric"),
        fields: IndexMap::from([
            (
                "name".into(),
                TypedFieldValue::from(Value::String("electric".into())),
            ),
            ("id".into(), TypedFieldValue::from(Value::Integer(13))),
        ]),
        relations: IndexMap::from([("pokemon".into(), vec![Ref::new("Pokemon", "pikachu")])]),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
    };
    {
        let mut guard = sess.lock_graph_cache().await;
        guard.insert(pikachu).expect("insert pikachu");
        guard.insert(electric.clone()).expect("insert electric");
    }
    let projected_row = serde_json::json!({"name": "electric"});
    let snapshot = super::plan_prefer_from_parent_get(
        &sess,
        &type_rel,
        "pokemon",
        "Pokemon",
        std::slice::from_ref(&electric),
        std::slice::from_ref(&projected_row),
    )
    .await
    .expect("plan prefer");
    let embedded = snapshot
        .all_embedded
        .expect("graph parent pokemon refs should fully embed");
    assert_eq!(embedded.len(), 1);
    assert_eq!(embedded[0].reference.primary_slot_str(), "pikachu");
}
