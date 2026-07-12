use super::super::*;

#[test]
fn materialized_result_use_preserves_scalar_data_binding_value() {
    let node = PlanNodeId::new("workspace_id".to_string()).expect("node id");
    let row = serde_json::json!("workspace_123");
    let entities = json_rows_to_entities("PlanComputed_workspace_id", std::slice::from_ref(&row));
    let mut materialized = BTreeMap::new();
    materialized.insert(
        node.clone(),
        MaterializedNode {
            entry_id: "acme".to_string(),
            entity: "PlanComputed_workspace_id".to_string(),
            result: Arc::new(ExecutionResult {
                count: entities.len(),
                entities: entities.clone(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: 0,
                    cache_hits: 0,
                    cache_misses: 0,
                    ..Default::default()
                },
                request_fingerprints: vec![],
            }),
            row_source: MaterializedRowSource::Inline(vec![row.clone()]),
            row_identities: vec![None],
            artifact: None,
            display: "workspace_id".to_string(),
            projection: None,
        },
    );
    let inputs = materialized_result_use_inputs(
        &materialized,
        &[PlanResultUse {
            node: node.as_str().to_string(),
            r#as: "workspace_id".to_string(),
        }],
        None,
    )
    .expect("inputs");
    let alias = InputAlias::new("workspace_id".to_string()).expect("alias");
    assert_eq!(inputs.get(&alias).expect("workspace_id").row, row);
}

#[test]
fn scoped_node_symbols_evaluate_against_singleton_inputs() {
    let value = PlanValue::Object {
        fields: BTreeMap::from([
            (
                "title".to_string(),
                PlanValue::Template {
                    template: "${p.name} uses ${moveFacts.move}".to_string(),
                    input_bindings: vec![],
                },
            ),
            (
                "power".to_string(),
                PlanValue::NodeSymbol {
                    node: "moveFacts".to_string(),
                    alias: "moveFacts".to_string(),
                    path: vec!["power".to_string()],
                },
            ),
        ]),
    };
    let row = serde_json::json!({ "name": "pikachu" });
    let inputs = BTreeMap::from([(
        InputAlias::new("moveFacts".to_string()).expect("alias"),
        MaterializedInputRow {
            node: PlanNodeId::new("moveFacts".to_string()).expect("node id"),
            proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
            row: serde_json::json!({ "move": "thunderbolt", "power": 90 }),
            rows: vec![serde_json::json!({ "move": "thunderbolt", "power": 90 })],
            row_identity: None,
            row_identities: vec![None],
        },
    )]);
    let binding = BindingName::new("p".to_string()).expect("binding");
    let scope = EvalScope::Bound {
        row: &row,
        binding: &binding,
    };
    let input_env = InputEnv { rows: &inputs };
    let env = PlanEvalEnv {
        scope,
        inputs: input_env,
        wire_coercion: None,
    };
    let out = eval_plan_value(&value, &env).expect("eval");
    assert_eq!(out["title"], "pikachu uses thunderbolt");
    assert_eq!(out["power"], 90);
}

#[test]
fn for_each_cross_uses_materialization_wires_upstream_singleton() {
    let plan = parse_plan_value(&serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "cross-binding-for-each",
        "nodes": [
            {
                "id": "find",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "data": { "kind": "literal", "value": [{ "id": "p1", "title": "Bolt" }] }
            },
            {
                "id": "report",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "single",
                "data": { "kind": "literal", "value": { "content": "STATS" } }
            },
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": "find",
                "item_binding": "_",
                "depends_on": ["find", "report"],
                "uses_result": [
                    { "node": "find", "as": "_" },
                    { "node": "report", "as": "report" }
                ],
                "effect_template": {
                    "kind": "action",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr_template": "Product.create(title=<<T\n${_.title} ${report.content}\nT\n)",
                    "ir_template": {
                        "expr": {
                            "op": "create",
                            "capability": "product_create",
                            "entity": "Product",
                            "input": { "title": "<<T\n${_.title} ${report.content}\nT\n" }
                        },
                        "input_bindings": []
                    },
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack"
                }
            }
        ],
        "return": { "kind": "node", "node": "label" }
    }))
    .expect("parse");
    let validated = validate_plan_artifact(&plan).expect("validate");
    let for_each = validated
        .nodes()
        .iter()
        .find_map(|node| match node {
            ValidatedPlanNode::ForEach(node) => Some(node),
            _ => None,
        })
        .expect("for_each");
    let mut materialized = BTreeMap::new();
    materialized.insert(
        PlanNodeId::new("report").expect("report"),
        MaterializedNode {
            entry_id: "acme".into(),
            entity: "Report".into(),
            result: Arc::new(ExecutionResult {
                entities: vec![],
                count: 1,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: 0,
                    cache_hits: 0,
                    cache_misses: 0,
                    ..Default::default()
                },
                request_fingerprints: vec![],
            }),
            row_source: MaterializedRowSource::Inline(vec![
                serde_json::json!({"content": "STATS"}),
            ]),
            row_identities: vec![None],
            artifact: None,
            display: String::new(),
            projection: None,
        },
    );
    let input_rows =
        materialized_result_use_inputs(&materialized, &for_each_cross_uses(for_each), None)
            .expect("input rows");
    assert_eq!(input_rows.len(), 1);
    let row = serde_json::json!({"id": "p1", "title": "Bolt"});
    let env = for_each_plan_eval_env(for_each, &row, &input_rows);
    let out =
        instantiate_expr_template_value(&serde_json::json!("${_.title} ${report.content}"), &env)
            .expect("interpolate");
    assert_eq!(out, serde_json::json!("Bolt STATS"));
}

#[test]
fn materialized_result_use_allows_plural_rows_for_column_node_input_holes() {
    let node = PlanNodeId::new("labels".to_string()).expect("node id");
    let rows = vec![
        serde_json::json!({"name": "bug"}),
        serde_json::json!({"name": "docs"}),
    ];
    let mut materialized = BTreeMap::new();
    materialized.insert(
        node.clone(),
        MaterializedNode {
            entry_id: "github".to_string(),
            entity: "Label".to_string(),
            result: Arc::new(ExecutionResult {
                count: rows.len(),
                entities: Vec::new(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            }),
            row_source: MaterializedRowSource::Inline(rows.clone()),
            row_identities: vec![None; rows.len()],
            artifact: None,
            display: String::new(),
            projection: Some(vec!["name".into()]),
        },
    );
    let template = ValidatedPlanExprTemplate {
        expr: serde_json::json!({
            "__plasm_hole": {
                "kind": "node_input",
                "alias": "labels",
                "path": ["name"]
            }
        }),
        projection: None,
        display_expr: None,
        input_bindings: vec![],
    };
    let input_rows = materialized_result_use_inputs(
        &materialized,
        &[PlanResultUse {
            node: node.as_str().to_string(),
            r#as: "labels".to_string(),
        }],
        Some(&template),
    )
    .expect("plural column projection inputs");
    let alias = InputAlias::new("labels".to_string()).expect("alias");
    assert_eq!(input_rows.get(&alias).expect("labels").rows.len(), 2);
    let env = PlanEvalEnv {
        scope: EvalScope::Root {
            row: &serde_json::Value::Null,
        },
        inputs: InputEnv { rows: &input_rows },
        wire_coercion: None,
    };
    let out =
        instantiate_expr_template_value(&template.expr, &env).expect("instantiate column array");
    assert_eq!(out, serde_json::json!(["bug", "docs"]));
}

#[tokio::test]
async fn view_embed_materialize_errors_without_view_produced_relation_refs() {
    let s = super::support::matrix_views_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "orphan-view-embed-parent",
        "nodes": [
            {
                "id": "raw",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "single",
                "data": {
                    "kind": "literal",
                    "value": {
                        "item_id": "i1",
                        "echo_title": "Item",
                        "tag_count": 0,
                        "has_tags": false
                    }
                }
            },
            {
                "id": "ctx",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "single",
                "compute": {
                    "source": "raw",
                    "op": {
                        "kind": "project",
                        "fields": {
                            "item_id": ["item_id"],
                            "echo_title": ["echo_title"],
                            "tag_count": ["tag_count"],
                            "has_tags": ["has_tags"]
                        }
                    },
                    "schema": {
                        "entity": "LangTriageContext",
                        "fields": [
                            { "name": "item_id", "value_kind": "string", "source": ["item_id"] },
                            { "name": "echo_title", "value_kind": "string", "source": ["echo_title"] },
                            { "name": "tag_count", "value_kind": "integer", "source": ["tag_count"] },
                            { "name": "has_tags", "value_kind": "boolean", "source": ["has_tags"] }
                        ]
                    }
                },
                "depends_on": ["raw"]
            },
            {
                "id": "tags",
                "kind": "relation",
                "effect_class": "read",
                "result_shape": "list",
                "relation": {
                    "source": "ctx",
                    "relation": "tags",
                    "target": { "entry_id": "langmatrix_views", "entity": "LangTag" },
                    "cardinality": "many",
                    "source_cardinality": "single",
                    "expr": "LangTriageContext(\"i1\").tags",
                    "materialize": {
                        "kind": "view_embed",
                        "view": "lang_triage_context"
                    },
                    "ir": {
                        "expr": {
                            "op": "chain",
                            "source": {
                                "op": "get",
                                "ref": { "entity_type": "LangTriageContext", "key": "i1" }
                            },
                            "selector": "tags",
                            "step": { "type": "auto_get" }
                        }
                    }
                },
                "depends_on": ["ctx"],
                "uses_result": [{ "node": "ctx", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "tags" }
    });
    let err = evaluate_plasm_plan_dry(&s, &plan)
        .expect_err("orphan view_embed must fail before run_ref");
    assert!(
        err.contains("view_embed_proof") || err.contains("view-produced"),
        "expected view_embed validation error, got: {err}"
    );
}
