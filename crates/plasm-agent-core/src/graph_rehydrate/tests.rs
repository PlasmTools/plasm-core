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
                    &page_entities.clone().into(),
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
        "schema_version": crate::session_graph_persistence::GRAPH_PAGE_DELTA_SCHEMA_VERSION,
        "entity_type": "Berry",
        "page_index": 0,
        "entities": rows,
    });
    let payload = crate::run_artifacts::ArtifactPayload {
        metadata: crate::run_artifacts::ArtifactPayloadMetadata {
            content_type: "application/json".into(),
            content_encoding: None,
            schema_version: crate::run_artifacts::RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION,
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
async fn recorded_rehydration_preserves_duplicates_across_overlapping_pages() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("cheri")]).await;
    fx.spill(&[
        vec![berry_entity("cheri"), berry_entity("pecha")],
        vec![berry_entity("unrelated")],
    ])
    .await;
    let rows =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", &berry_membership(&["pecha", "cheri", "pecha"]))
            .await
            .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], rows[2]);
    assert_ne!(rows[0], rows[1]);
}

#[tokio::test]
async fn recorded_rehydration_rejects_untyped_spill_rows() {
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

    let error =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", &berry_membership(&["cheri"]))
            .await
            .expect_err("missing typed identity");
    assert!(matches!(
        error,
        super::walk::GraphRehydrateError::Persistence(
            crate::session_graph_persistence::SessionGraphPersistenceError::Serialization(_)
        )
    ));
}

#[tokio::test]
async fn materialized_entities_use_walker_when_persistence_exists() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("cheri")]).await;
    fx.spill(&[vec![berry_entity("cheri"), berry_entity("pecha")]])
        .await;

    let result = plasm_runtime::ExecutionResult {
        collection: plasm_runtime::execution::ExecutionCollection::graph(berry_membership(&[
            "pecha", "cheri", "pecha",
        ])),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: plasm_runtime::ExecutionSource::Live,
        stats: plasm_runtime::ExecutionStats::default(),
        request_fingerprints: Vec::new(),
        operations: plasm_runtime::OperationLedger::empty(),
    };
    let direct =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", &berry_membership(&["pecha", "cheri"]))
            .await
            .expect("rehydrate rows");
    assert_eq!(direct.len(), 2);

    let entities =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .materialize_entities_for_result("Berry", &result)
            .await;

    assert_eq!(entities.len(), 3);
    assert!(std::ptr::eq(&entities[0], &entities[2]));
    assert_eq!(entities[0].reference.primary_slot_str(), "pecha");
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
        relations: IndexMap::from([("tags".into(), vec![tag_ref])])
            .into_iter()
            .map(|(key, refs)| {
                (
                    key,
                    plasm_core::row_contract::RelationMembership::observe(
                        None,
                        &"relation_fixture",
                        refs,
                        None,
                    )
                    .unwrap(),
                )
            })
            .collect(),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
        unavailable_fields: Default::default(),
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
        unavailable_fields: Default::default(),
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
        collection: crate::test_support::execution_fixtures::collection(
            vec![projected],
            plasm_runtime::ResultCoverage::Unknown,
        ),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
        operations: plasm_runtime::OperationLedger::empty(),
    };
    let host = SpillHostFixture::new();
    let parents = super::GraphSurfaceRehydrator::new(
        &sess,
        host.st.as_ref(),
        PROJECT_THEN_RELATE_SID,
        cgs.as_ref(),
    )
    .resolve_source_parents_with_identities("LangItem", &result, &[Some(row_identity)])
    .await
    .unwrap();
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
        unavailable_fields: Default::default(),
    };
    let row_identity = RowIdentity::new(
        QualifiedEntityKey::new("default", "LangItem"),
        item_ref,
        IndexMap::new(),
        IdEncoding::Simple,
    );
    let result = ExecutionResult {
        collection: crate::test_support::execution_fixtures::collection(
            vec![projected],
            plasm_runtime::ResultCoverage::Unknown,
        ),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
        operations: plasm_runtime::OperationLedger::empty(),
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
        parents.is_err(),
        "identity-bound rows must not fall back to thin projected entities when graph parent is missing"
    );
}

#[tokio::test]
async fn membership_evidence_survives_hot_and_spilled_entity_walks() {
    let fx = GraphRehydrateFixture::new();
    let mut parent = berry_entity("cheri");
    parent.relations.insert("children".into(), {
        use plasm_core::collection_codec::{
            CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
        };
        let references = vec![
            plasm_core::Ref::new("Berry", "pecha"),
            plasm_core::Ref::new("Berry", "pecha"),
        ];
        let observation = Observation::ExactOutput {
            decoded: references.len(),
        };
        plasm_core::row_contract::RelationMembership::from_record(
            RecordingCodec::new()
                .record(
                    CollectionIdentity::for_untyped_observation(&"relation_fixture").unwrap(),
                    references,
                    observation,
                )
                .unwrap(),
        )
    });
    fx.spill(&[vec![parent.clone()]]).await;
    let rehydrator =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref());
    let mut result = plasm_runtime::ExecutionResult {
        collection: crate::test_support::execution_fixtures::collection(
            vec![],
            plasm_runtime::ResultCoverage::Complete,
        ),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: plasm_runtime::ExecutionSource::Live,
        stats: Default::default(),
        request_fingerprints: vec![],
        operations: Default::default(),
    };
    result.collection = plasm_runtime::execution::ExecutionCollection::graph(
        crate::test_support::execution_fixtures::collection(
            vec![parent.clone()],
            plasm_runtime::ResultCoverage::Complete,
        )
        .membership()
        .clone(),
    );
    let recovered = rehydrator
        .resolve_source_parents("Berry", &result)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].relations, parent.relations);
    fx.insert_hot(&[parent.clone()]).await;
    let hot = rehydrator
        .resolve_source_parents("Berry", &result)
        .await
        .unwrap();
    assert_eq!(hot[0].relations, parent.relations);
}

fn berry_membership(
    names: &[&str],
) -> plasm_core::collection_codec::RecordedCollection<plasm_core::Ref> {
    use plasm_core::collection_codec::{
        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
    };
    RecordingCodec::new()
        .record(
            CollectionIdentity::for_untyped_observation(&"berries").unwrap(),
            names
                .iter()
                .map(|name| plasm_core::Ref::new("Berry", *name))
                .collect(),
            Observation::ExactOutput {
                decoded: names.len(),
            },
        )
        .unwrap()
}

#[tokio::test]
async fn absent_recorded_member_is_an_error_even_when_cache_count_matches() {
    let fx = GraphRehydrateFixture::new();
    fx.insert_hot(&[berry_entity("unrelated")]).await;
    let membership = berry_membership(&["missing"]);
    let result =
        super::GraphSurfaceRehydrator::new(&fx.es, fx.host.st.as_ref(), SID, fx.cgs.as_ref())
            .rehydrate_rows_locked("Berry", &membership)
            .await;
    assert!(result.is_err());
}
