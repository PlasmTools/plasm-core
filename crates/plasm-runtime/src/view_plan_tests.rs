//! Unit tests for [`super::view_plan`] (extracted to keep `view_plan.rs` under 1k lines).

use super::*;

use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{current_timestamp, ExecutionResult, ExecutionSource, ExecutionStats};
use crate::view_dag_run::run_view_dag_sync;
use crate::view_test_support::{lang_digest_scope, matrix_views_cgs};
use plasm_compile::DecodedRelation;
use plasm_core::{QueryExpr, TypedFieldValue, Value, CGS};

fn stub_item_row(id: &str, title: &str) -> CachedEntity {
    let mut fields = indexmap::IndexMap::new();
    fields.insert("id".into(), TypedFieldValue::String(id.into()));
    fields.insert("title".into(), TypedFieldValue::String(title.into()));
    fields.insert("score".into(), TypedFieldValue::Integer(0));
    fields.insert("owner".into(), TypedFieldValue::String(String::new()));
    CachedEntity::from_decoded(
        plasm_core::Ref::new("LangItem", id),
        fields.into_iter().map(|(k, v)| (k, v.to_value())).collect(),
        indexmap::IndexMap::new(),
        current_timestamp(),
        EntityCompleteness::Complete,
    )
}

#[test]
fn derive_view_query_scope_missing_predicate_errors() {
    use plasm_core::identity::EntityName;
    use plasm_core::schema::{ViewDefinition, ViewScopeParam};

    let mut cgs = CGS::new();
    cgs.views.insert(
        "needs_pred".into(),
        ViewDefinition {
            description: String::new(),
            capability: "cap".into(),
            entity: EntityName::new("LangDigest"),
            scope: vec![ViewScopeParam {
                name: "item_id".into(),
                value_ref: None,
                required: true,
                inject: None,
            }],
            nodes: vec![],
            output: indexmap::IndexMap::new(),
            relation_outputs: vec![],
        },
    );
    let query = QueryExpr::all("LangDigest");
    let err = derive_view_query_scope("needs_pred", &query, &cgs).expect_err("scope");
    assert!(
        err.to_string().contains("requires a query predicate"),
        "{err}"
    );
}

#[test]
fn merge_ambient_scope_uses_explicit_transport_origin() {
    use plasm_core::identity::EntityName;
    use plasm_core::schema::{ViewDefinition, ViewScopeInject, ViewScopeParam};

    let view = ViewDefinition {
        description: String::new(),
        capability: "cap".into(),
        entity: EntityName::new("LangDigest"),
        scope: vec![ViewScopeParam {
            name: "origin".into(),
            value_ref: None,
            required: false,
            inject: Some(ViewScopeInject::SessionTransportOrigin),
        }],
        nodes: vec![],
        output: indexmap::IndexMap::new(),
        relation_outputs: vec![],
    };
    let mut scope = indexmap::IndexMap::new();
    let ambient = ViewAmbientContext {
        transport_origin: Some("https://host.test".into()),
        ui_origin: None,
    };
    merge_view_ambient_scope(&view, &mut scope, &ambient);
    assert_eq!(
        scope.get("origin"),
        Some(&Value::String("https://host.test".into()))
    );
}

fn stub_item_node_result(id: &str, title: &str) -> ExecutionResult {
    ExecutionResult {
        entities: vec![stub_item_row(id, title)],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats::default(),
        request_fingerprints: vec![],
    }
}

#[test]
fn preflight_proof_matches_fixture_runner_on_lang_digest() {
    let cgs = matrix_views_cgs();
    let scope = lang_digest_scope();
    let ambient = ViewAmbientContext::default();
    let preflight_proof = crate::view_preflight::preflight_view_scoped_with_proof(
        "lang_digest",
        scope.clone(),
        &cgs,
        &ambient,
    )
    .expect("preflight proof");
    assert_eq!(
        preflight_proof.output_fields.get("echo_title"),
        Some(&Value::String(String::new()))
    );
    assert_eq!(
        preflight_proof.output_fields.get("echo_slug"),
        Some(&Value::String("item-1-".into()))
    );

    let results =
        indexmap::IndexMap::from([("item_node".into(), stub_item_node_result("item-1", ""))]);
    let (fixture_proof, _) = run_view_dag_sync(
        &FixtureViewNodeRunner { results },
        "lang_digest",
        scope,
        &cgs,
        &ambient,
    )
    .expect("fixture");
    assert_eq!(
        preflight_proof.output_fields.get("echo_title"),
        fixture_proof.output_fields.get("echo_title")
    );
    assert_eq!(
        preflight_proof.output_fields.get("echo_slug"),
        fixture_proof.output_fields.get("echo_slug")
    );
}

#[test]
fn fixture_runner_relation_cardinality_single_row() {
    let cgs = matrix_views_cgs();
    let runner = FixtureViewNodeRunner {
        results: indexmap::IndexMap::from([(
            "item_node".into(),
            stub_item_node_result("item-1", "Alpha"),
        )]),
    };
    let scope = lang_digest_scope();
    let ambient = ViewAmbientContext::default();
    let (proof, _) =
        run_view_dag_sync(&runner, "lang_digest", scope, &cgs, &ambient).expect("fixture run");
    assert_eq!(
        proof.output_fields.get("echo_title"),
        Some(&Value::String("Alpha".into()))
    );
    assert!(proof.relation_refs.contains_key("item_snapshot"));
}

#[test]
fn fixture_runner_node_single_row_cardinality_error() {
    let cgs = matrix_views_cgs();
    let mut results = indexmap::IndexMap::new();
    results.insert(
        "item_node".into(),
        ExecutionResult {
            entities: vec![stub_item_row("a", "A"), stub_item_row("b", "B")],
            count: 2,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Cache,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
        },
    );
    let runner = FixtureViewNodeRunner { results };
    let scope = lang_digest_scope();
    let ambient = ViewAmbientContext::default();
    let err =
        run_view_dag_sync(&runner, "lang_digest", scope, &cgs, &ambient).expect_err("cardinality");
    assert!(err.to_string().contains("node_single_row"), "{err}");
}

#[test]
fn resolve_view_relation_maps_stamps_empty_many_relation_key() {
    let cgs = matrix_views_cgs();
    let view = cgs.views.get("lang_work_snapshot_empty").expect("view");
    let node_results = indexmap::IndexMap::from([
        (
            "viewer_row".into(),
            ExecutionResult {
                entities: vec![CachedEntity::from_decoded(
                    plasm_core::Ref::new("LangViewer", "viewer-nobody"),
                    indexmap::IndexMap::from([
                        (
                            "id".into(),
                            plasm_core::Value::String("viewer-nobody".into()),
                        ),
                        (
                            "display_name".into(),
                            plasm_core::Value::String("nobody".into()),
                        ),
                    ]),
                    indexmap::IndexMap::new(),
                    current_timestamp(),
                    EntityCompleteness::Complete,
                )],
                count: 1,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            },
        ),
        (
            "assigned_items".into(),
            ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            },
        ),
    ]);
    let maps = resolve_view_relation_maps(view, &node_results, &cgs).expect("relation maps");
    assert!(
        maps.contains_key("items"),
        "many-valued relation_outputs must retain key even when empty: {maps:?}"
    );
    match maps.get("items") {
        Some(DecodedRelation::Specified(refs)) => assert!(refs.is_empty()),
        other => panic!("expected Specified([]), got {other:?}"),
    }
}

pub struct FixtureViewNodeRunner {
    pub results: indexmap::IndexMap<String, ExecutionResult>,
}

impl ViewNodeRunner for FixtureViewNodeRunner {
    fn run_query_node(
        &self,
        _ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        _cap: &plasm_core::CapabilitySchema,
        _pred: &plasm_core::Predicate,
        _node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, crate::RuntimeError> {
        self.results
            .get(&node.id)
            .cloned()
            .ok_or_else(|| crate::RuntimeError::ConfigurationError {
                message: format!("fixture runner missing node `{}`", node.id),
            })
    }

    fn run_get_node(
        &self,
        _ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        _cap: &plasm_core::CapabilitySchema,
        _get: &plasm_core::GetExpr,
        _bound: &std::collections::BTreeMap<String, String>,
    ) -> Result<ExecutionResult, crate::RuntimeError> {
        self.results
            .get(&node.id)
            .cloned()
            .ok_or_else(|| crate::RuntimeError::ConfigurationError {
                message: format!("fixture runner missing node `{}`", node.id),
            })
    }

    fn run_create_node(
        &self,
        _ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        _cap: &plasm_core::CapabilitySchema,
        _create: &plasm_core::CreateExpr,
    ) -> Result<ExecutionResult, crate::RuntimeError> {
        self.results
            .get(&node.id)
            .cloned()
            .ok_or_else(|| crate::RuntimeError::ConfigurationError {
                message: format!("fixture runner missing node `{}`", node.id),
            })
    }
}
