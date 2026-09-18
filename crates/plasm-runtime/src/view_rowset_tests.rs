//! Abstract rowset-view laws exercised through the live transport boundary.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::{ChainExpr, Expr, Predicate, QueryExpr, Value};

use crate::auth::ResolvedAuth;
use crate::execution::{ExecutionMode, ResultCoverage, StreamConsumeOpts};
use crate::http_transport::HttpTransport;
use crate::{ExecutionConfig, ExecutionEngine, RuntimeError, SessionMaterialization};

fn schema() -> plasm_core::CGS {
    plasm_core::loader::load_schema_dir(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/view_rowsets"),
    )
    .expect("abstract rowset view")
}

proptest::proptest! {
    #[test]
    fn view_rowsets_branch_snapshot_replacement(
        old in proptest::collection::vec(0u8..16, 0..16),
        new in proptest::collection::vec(0u8..16, 0..16),
        fresh_first in proptest::bool::ANY,
    ) {
        let reference = plasm_core::Ref::new("Library", "test-token");
        let row = |ids: &[u8]| crate::CachedEntity::from_decoded(
            reference.clone(),
            [("access_token".into(), Value::String("test-token".into()))].into(),
            [("items".into(), plasm_compile::DecodedRelation::Specified(ids.iter().map(|id| plasm_core::Ref::new("Item", format!("i{id}"))).collect()))].into(),
            0,
            crate::EntityCompleteness::Complete,
        );
        let mut parent = SessionMaterialization::new();
        parent.publish_fresh_row(row(&old)).unwrap();
        let (mut fresh, _) = crate::BranchMaterializationBase::fork_from(&parent);
        let (mut sibling, _) = crate::BranchMaterializationBase::fork_from(&parent);
        fresh.publish_fresh_row(row(&new)).unwrap();
        sibling.insert(crate::CachedEntity::from_decoded(
            plasm_core::Ref::new("Item", "unrelated"),
            [("title".into(), Value::String("unrelated".into()))].into(),
            Default::default(), 0, crate::EntityCompleteness::Complete,
        )).unwrap();
        if fresh_first {
            parent.absorb_branch(fresh).unwrap();
            parent.absorb_branch(sibling).unwrap();
        } else {
            parent.absorb_branch(sibling).unwrap();
            parent.absorb_branch(fresh).unwrap();
        }
        let expected = row(&new);
        proptest::prop_assert_eq!(&parent.get(&reference).unwrap().relations, &expected.relations);
        proptest::prop_assert!(parent.get(&plasm_core::Ref::new("Item", "unrelated")).is_some());
    }
}

struct Transport {
    paths: Arc<Mutex<Vec<String>>>,
    reverse: bool,
    fault: Option<Fault>,
}

#[derive(Clone, Copy)]
enum Fault {
    MissingRelation,
    WrongIdentity,
}

#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _base: &str,
        request: &CompiledRequest,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.paths.lock().unwrap().push(request.path.clone());
        let Some(Value::Object(headers)) = &request.headers else {
            panic!("credential header absent")
        };
        assert_eq!(
            headers.get("Authorization"),
            Some(&Value::String("test-token".into()))
        );
        if request.path.trim_matches('/') == "collections/c1" {
            match self.fault {
                Some(Fault::MissingRelation) => return Ok((serde_json::json!({"id":"c1"}), None)),
                Some(Fault::WrongIdentity) => {
                    return Ok((
                        serde_json::json!({"id":"other","items":[{"id":"i3"}]}),
                        None,
                    ))
                }
                None => {}
            }
        }
        let body = match request.path.trim_matches('/') {
            "items" => serde_json::json!([{"id":"i1", "title":"one"}]),
            "collections" => {
                let mut ids = vec!["c1", "c2", "empty"];
                if self.reverse {
                    ids.reverse();
                }
                let page = match &request.query {
                    Some(Value::Object(query)) => match query.get("page_index") {
                        Some(Value::Integer(page)) => *page as usize,
                        _ => 0,
                    },
                    _ => 0,
                };
                serde_json::json!(ids
                    .into_iter()
                    .skip(page * 2)
                    .take(2)
                    .map(|id| serde_json::json!({"id":id}))
                    .collect::<Vec<_>>())
            }
            "collections/c1" => serde_json::json!({"id":"c1","items":[{"id":"i1"},{"id":"i2"}]}),
            "collections/c2" => serde_json::json!({"id":"c2","items":[{"id":"i2"},{"id":"i3"}]}),
            "collections/empty" => serde_json::json!({"id":"empty","items":[]}),
            "items/i1" => serde_json::json!({"id":"i1", "title":"one"}),
            "items/i2" => serde_json::json!({"id":"i2", "title":"two"}),
            "items/i3" => serde_json::json!({"id":"i3", "title":"three"}),
            other => panic!("unexpected request {other}"),
        };
        Ok((body, None))
    }
    async fn get_json_absolute(
        &self,
        _url: &str,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("absolute requests not expected")
    }
}

#[tokio::test]
async fn view_rowsets_live_traversal_union_identity_scope_and_order() {
    for reverse in [false, true] {
        let cgs = schema();
        plasm_compile::validate_cgs_views(&cgs).expect("view validation");
        let compiled =
            Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).expect("compile"));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://rowsets.test".into()),
                ..Default::default()
            },
            Arc::new(Transport {
                paths: paths.clone(),
                reverse,
                fault: None,
            }),
            None,
        );
        let query = QueryExpr::filtered("Library", Predicate::eq("access_token", "test-token"));
        let mut mat = SessionMaterialization::new();
        mat.insert(crate::CachedEntity::from_decoded(
            plasm_core::Ref::new("Library", "test-token"),
            [("access_token".into(), Value::String("test-token".into()))].into(),
            [(
                "items".into(),
                plasm_compile::DecodedRelation::Specified(vec![plasm_core::Ref::new(
                    "Item", "obsolete",
                )]),
            )]
            .into(),
            0,
            crate::EntityCompleteness::Complete,
        ))
        .unwrap();
        let result = engine
            .execute(
                &Expr::Chain(ChainExpr::auto_get(Expr::Query(query), "items")),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                crate::execution::ExecuteOptions {
                    compiled_catalog: Some(compiled),
                    ..Default::default()
                },
            )
            .await
            .expect("composed traversal");
        let ids: std::collections::BTreeSet<_> = result
            .entities
            .iter()
            .map(|row| row.reference.primary_slot_str().to_string())
            .collect();
        assert_eq!(
            ids,
            std::collections::BTreeSet::from(["i1".into(), "i2".into(), "i3".into()])
        );
        assert_eq!(
            result.entities.len(),
            3,
            "union must remove overlapping identities"
        );
        assert_eq!(result.coverage, ResultCoverage::Complete);
        let parent = mat
            .get(&plasm_core::Ref::new("Library", "test-token"))
            .expect("query-produced view parent must be graph-bound");
        assert_eq!(parent.relations.get("items").unwrap().len(), 3);
        for row in &result.entities {
            assert!(
                matches!(row.fields.get("title"), Some(plasm_core::TypedFieldValue::String(title)) if !title.is_empty()),
                "union child must retain hydrated details: {}",
                row.reference
            );
        }
        let paths = paths.lock().unwrap();
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.trim_matches('/') == "collections")
                .count(),
            2
        );
        for id in ["c1", "c2", "empty"] {
            assert!(paths
                .iter()
                .any(|p| p.trim_matches('/') == format!("collections/{id}")));
        }
    }
}

#[test]
fn view_rowsets_validation_rejects_bad_dependencies_and_union_types() {
    let cgs = schema();
    cgs.validate().expect("valid view");
    for source in ["missing", "nested"] {
        let mut cgs = schema();
        cgs.views["library"].nodes[2]
            .traverse
            .as_mut()
            .unwrap()
            .node = source.into();
        assert!(cgs.validate().is_err());
    }
    let mut cgs = schema();
    cgs.views["library"].nodes[2]
        .traverse
        .as_mut()
        .unwrap()
        .relation = "missing".into();
    assert!(cgs.validate().is_err());
    let mut cgs = schema();
    cgs.views["library"].relation_outputs[0].binding =
        plasm_core::schema::ViewRelationBinding::NodeUnionRows {
            nodes: vec!["collections".into()],
        };
    assert!(cgs.validate().is_err());
}

#[tokio::test]
async fn view_rowsets_unavailable_and_wrong_identity_do_not_become_empty_complete() {
    for fault in [Fault::MissingRelation, Fault::WrongIdentity] {
        let cgs = schema();
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://rowsets.test".into()),
                ..Default::default()
            },
            Arc::new(Transport {
                paths: Arc::new(Mutex::new(Vec::new())),
                reverse: false,
                fault: Some(fault),
            }),
            None,
        );
        let query = QueryExpr::filtered("Library", Predicate::eq("access_token", "test-token"));
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                crate::execution::ExecuteOptions {
                    compiled_catalog: Some(compiled),
                    ..Default::default()
                },
            )
            .await;
        let error = result
            .expect_err("unestablished traversal must surface an error")
            .to_string();
        let expected = match fault {
            Fault::MissingRelation => "did not establish relation",
            Fault::WrongIdentity => "identity mismatch",
        };
        assert!(error.contains(expected), "unexpected boundary: {error}");
        assert!(
            mat.get(&plasm_core::Ref::new("Library", "test-token"))
                .is_none(),
            "failed composition must not publish its parent"
        );
        assert!(
            mat.get(&plasm_core::Ref::new("Collection", "other"))
                .is_none(),
            "divergent parent must not enter cache"
        );
    }
}

#[test]
fn view_rowsets_preflight_compiles_inner_materialization() {
    let cgs = schema();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    crate::preflight_view_query(
        "library",
        &QueryExpr::filtered("Library", Predicate::eq("access_token", "test-token")),
        &cgs,
        &compiled,
        &crate::ViewAmbientContext::default(),
        &SessionMaterialization::new(),
    )
    .expect("preflight must compile parent and child GETs with invocation credentials");
}

struct MutableCollectionTransport {
    cleared: Mutex<bool>,
    base: Transport,
}

#[async_trait]
impl HttpTransport for MutableCollectionTransport {
    async fn send_compiled_http(
        &self,
        origin: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        if request.path.trim_matches('/') == "collections/c1/items" {
            assert_eq!(request.method, plasm_compile::HttpMethod::Delete);
            *self.cleared.lock().unwrap() = true;
            return Ok((serde_json::json!({"success": true}), None));
        }
        if request.path.trim_matches('/') == "collections/c1" && *self.cleared.lock().unwrap() {
            return Ok((serde_json::json!({"id":"c1", "items":[]}), None));
        }
        self.base.send_compiled_http(origin, request, auth).await
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("no absolute requests")
    }
}

#[tokio::test]
async fn mutation_refreshes_collection_membership_inside_view() {
    let cgs = schema();
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://rowsets.test".into()),
            ..Default::default()
        },
        Arc::new(MutableCollectionTransport {
            cleared: Mutex::new(false),
            base: Transport {
                paths: Arc::new(Mutex::new(Vec::new())),
                reverse: false,
                fault: None,
            },
        }),
        None,
    );
    let mut mat = SessionMaterialization::new();
    let query = Expr::Query(QueryExpr::filtered(
        "Library",
        Predicate::eq("access_token", "test-token"),
    ));
    for expression in [
        query.clone(),
        Expr::Invoke(plasm_core::InvokeExpr::new(
            "collection_clear",
            "Collection",
            "c1",
            None,
        )),
        query,
    ] {
        engine
            .execute(
                &expression,
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                crate::execution::ExecuteOptions {
                    compiled_catalog: Some(compiled.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let parent = mat.get(&plasm_core::Ref::new("Collection", "c1")).unwrap();
    assert_eq!(
        parent.relations.get("items"),
        Some(&Vec::new()),
        "view refresh must publish the empty post-write membership"
    );
    // c2 and directly saved items still contribute to the library.
    assert_eq!(
        mat.get(&plasm_core::Ref::new("Library", "test-token"))
            .unwrap()
            .relations["items"]
            .len(),
        3
    );
}
