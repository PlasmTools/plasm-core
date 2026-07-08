use super::super::*;
use super::support::*;
use crate::plan_flow_policy::{
    CapabilityGatePattern, CapabilityGateRule, OperatorDisposition, FlowPolicy, FlowPolicySnapshot,
    PolicyRevision,
};

#[test]
fn create_template_approval_uses_create_operation_not_description_text() {
    let mut s = test_session();
    // Active policy: require review for issue_create so the gate is emitted.
    s.flow_policy = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy: FlowPolicy {
            capability_gates: vec![CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: Some("linear".into()),
                    entity: Some("Issue".into()),
                    capability: "issue_create".into(),
                },
                enforcement: OperatorDisposition::Approve,
            }],
            ..FlowPolicy::default()
        },
    };
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "create-report",
        "nodes": [
            {
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "createIssue",
                "kind": "for_each",
                "effect_class": "write",
                "result_shape": "mutation_result",
                "source": "products",
                "item_binding": "product",
                "depends_on": ["products"],
                "uses_result": [{ "node": "products", "as": "product" }],
                "effect_template": {
                    "kind": "create",
                    "qualified_entity": { "entry_id": "linear", "entity": "Issue" },
                    "expr_template": "Issue.create(title=\"Report\", description=\"1.) text that looks like member syntax\")",
                    "ir_template": {
                        "expr": {
                            "op": "create",
                            "capability": "issue_create",
                            "entity": "Issue",
                            "input": {
                                "title": "Report",
                                "description": "1.) text that looks like member syntax"
                            }
                        },
                        "input_bindings": []
                    },
                    "effect_class": "write",
                    "result_shape": "mutation_result"
                }
            }
        ],
        "return": { "kind": "node", "node": "createIssue" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(
        dry.graph_summary["approval_gates"][0]["policy_key"],
        "linear.Issue.issue_create"
    );
    let text = render_plasm_plan_dry_text(&dry, None);
    assert!(
        !text.contains("approval:"),
        "dry-run text omits approval policy lines (auto-approved host policy): {text}"
    );
}

#[test]
fn mutating_for_each_infers_approval_without_agent_label() {
    let mut s = test_session();
    // Active policy: require review for product_label so the gate is emitted.
    s.flow_policy = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy: FlowPolicy {
            capability_gates: vec![CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: Some("acme".into()),
                    entity: Some("Product".into()),
                    capability: "product_label".into(),
                },
                enforcement: OperatorDisposition::Approve,
            }],
            ..FlowPolicy::default()
        },
    };
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "label-products",
        "nodes": [
            {
                "id": "find",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": "find",
                "item_binding": "product",
                "depends_on": ["find"],
                "uses_result": [{ "node": "find", "as": "product" }],
                "effect_template": {
                    "kind": "action",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr_template": "Product(${product.id}).label(label=\"stale\")",
                    "ir_template": {
                        "expr": {
                            "op": "invoke",
                            "capability": "product_label",
                            "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                            "input": { "label": "stale" }
                        },
                        "input_bindings": [{ "from": "product.id", "to": "id" }]
                    },
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack"
                }
            }
        ],
        "return": { "kind": "parallel", "nodes": ["find", "label"] }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(
        dry.graph_summary["approval_gates"][0]["policy_key"],
        "acme.Product.product_label"
    );
}

#[test]
fn mutating_surface_gate_with_approve_policy() {
    use crate::flow_catalog::FlowCatalogView;
    use crate::plan_flow::verify_plan_flow;
    use crate::plasm_plan::parse_and_validate_plan_json;

    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "c1",
            "kind": "create",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product.create(name=\"servo\")",
            "ir": { "expr": { "op": "create", "capability": "product_create", "entity": "Product", "input": { "name": "servo" } } },
            "effect_class": "write",
            "result_shape": "single"
        }],
        "return": { "kind": "node", "node": "c1" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");

    // Inactive policy: Allow disposition → no approval gate.
    let checked_inactive = verify_plan_flow(
        validated.artifact(),
        &["c1".to_string()],
        &FlowCatalogView::default(),
        &FlowPolicySnapshot::Inactive,
    );
    assert!(
        checked_inactive.analysis.approval_gate_for_node("c1").is_none(),
        "inactive policy must not emit approval gate"
    );

    // Active policy with Approve enforcement for "create" operations.
    let active_policy = FlowPolicy {
        capability_gates: vec![CapabilityGateRule {
            pattern: CapabilityGatePattern {
                entry_id: Some("acme".into()),
                entity: Some("Product".into()),
                // non-template Surface create: cap_name falls back to operation_name_for_kind → "create"
                capability: "create".into(),
            },
            enforcement: OperatorDisposition::Approve,
        }],
        ..FlowPolicy::default()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy: active_policy,
    };
    let checked = verify_plan_flow(validated.artifact(), &["c1".to_string()], &FlowCatalogView::default(), &snapshot);
    let gate = checked
        .analysis
        .approval_gate_for_node("c1")
        .expect("approval gate");
    assert_eq!(gate["policy_key"], "acme.Product.create");
    assert_eq!(gate["host_policy"], "host.review");
    assert_eq!(gate["default_decision"], "approved");
}

#[test]
fn automatic_approval_policy_emits_receipt_for_gate() {
    let gate = serde_json::json!({
        "node": "c1",
        "required": true,
        "policy_key": "acme.Product.create"
    });
    let receipt = PlasmPlanApprovalPolicy::automatic().review(gate.clone());
    let summary = graph_summary_with_approval_receipts(serde_json::json!({}), &[receipt]);

    assert_eq!(summary["approval_receipts"][0]["decision"], "approved");
    assert_eq!(
        summary["approval_receipts"][0]["policy"],
        "host.auto_approve"
    );
    assert_eq!(summary["approval_receipts"][0]["gate"], gate);
}

#[test]
fn for_each_plan_eval_env_interpolates_row_and_cross_binding_strings() {
    let row = serde_json::json!({"title": "Bolt"});
    let mut input_rows = BTreeMap::new();
    input_rows.insert(
        InputAlias::new("report".to_string()).expect("alias"),
        MaterializedInputRow {
            node: PlanNodeId::new("report").expect("node"),
            proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
            row: serde_json::json!({"content": "STATS"}),
            rows: vec![serde_json::json!({"content": "STATS"})],
            row_identity: None,
            row_identities: vec![None],
        },
    );
    let binding = BindingName::new("_".to_string()).expect("binding");
    let scope = EvalScope::Bound {
        row: &row,
        binding: &binding,
    };
    let inputs = InputEnv { rows: &input_rows };
    let env = PlanEvalEnv {
        scope,
        inputs,
        wire_coercion: None,
    };
    let out =
        instantiate_expr_template_value(&serde_json::json!("${_.title} / ${report.content}"), &env)
            .expect("interpolate");
    assert_eq!(out, serde_json::json!("Bolt / STATS"));
}
