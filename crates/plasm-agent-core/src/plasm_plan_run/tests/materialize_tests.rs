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
            row_identity: None,
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
    let input_rows = materialized_result_use_inputs(&materialized, &for_each_cross_uses(for_each))
        .expect("input rows");
    assert_eq!(input_rows.len(), 1);
    let row = serde_json::json!({"id": "p1", "title": "Bolt"});
    let env = for_each_plan_eval_env(for_each, &row, &input_rows);
    let out =
        instantiate_expr_template_value(&serde_json::json!("${_.title} ${report.content}"), &env)
            .expect("interpolate");
    assert_eq!(out, serde_json::json!("Bolt STATS"));
}
