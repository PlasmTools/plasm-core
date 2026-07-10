#![cfg(test)]

use super::*;
use crate::flow_catalog::FlowCatalogView;
use crate::plan_flow_policy::{FlowPolicy, ForbiddenFlowRule, OperatorDisposition};
use crate::plasm_plan::parse_and_validate_plan_json;

#[test]
fn flow_catalog_view_defaults_to_empty_sets() {
    let view = FlowCatalogView::default();
    let key = QualifiedCapabilityKey::from_parts("entry", "Entity", "action");
    assert!(view.output_labels_for(&key).is_empty());
    assert!(view.sink_params_for(&key).is_empty());
    assert!(view.sanitizers_for(&key).is_empty());
}

#[test]
fn render_compute_row_joins_all_input_labels() {
    let source = NodeFlowFacts {
        columns: BTreeMap::from([(
            vec!["body".into()],
            FlowFacts {
                labels: BTreeSet::from([DataClassName::new("untrusted").expect("label")]),
                provenance: BTreeSet::new(),
            },
        )]),
        residual: FlowFacts::default(),
    };
    assert!(source
        .row_join()
        .labels
        .contains(&DataClassName::new("untrusted").expect("label")));
}

#[test]
fn forbidden_untrusted_to_outbound_sink_denies_mutation() {
    let mut catalog = FlowCatalogView::default();
    let read_key = QualifiedCapabilityKey::from_parts("flow", "Message", "Message_query");
    let send_key = QualifiedCapabilityKey::from_parts("flow", "Message", "send");
    catalog.capability_output_labels.insert(
        read_key,
        BTreeSet::from([DataClassName::new("untrusted").expect("untrusted")]),
    );
    catalog.capability_sink_params.insert(
        send_key,
        vec![SinkParamRef {
            param: CapabilityParamName::from("body"),
            sink_class: Some(SinkClassName::new("outbound_body").expect("sink")),
        }],
    );

    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "messages",
                "kind": "query",
                "qualified_entity": { "entry_id": "flow", "entity": "Message" },
                "expr": "Message",
                "ir": { "expr": { "op": "query", "entity": "Message", "capability": "message_query" } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "send",
                "kind": "action",
                "qualified_entity": { "entry_id": "flow", "entity": "Message" },
                "depends_on": ["messages"],
                "uses_result": [{ "node": "messages", "as": "messages" }],
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "ir_template": {
                    "expr": {
                        "op": "invoke",
                        "capability": "send",
                        "target": { "entity_type": "Message", "key": { "id": "1" } },
                        "input": {
                            "body": { "__plasm_hole": { "kind": "node_input", "alias": "messages", "path": ["body"] } }
                        }
                    }
                }
            }
        ],
        "return": { "kind": "node", "node": "send" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");
    let topo = vec!["messages".to_string(), "send".to_string()];
    let policy = FlowPolicy {
        forbidden: vec![ForbiddenFlowRule {
            from_label: DataClassName::new("untrusted").expect("untrusted"),
            to_sink: Some(SinkClassName::new("outbound_body").expect("sink")),
            reason: Some("untrusted cannot reach outbound body".into()),
        }],
        ..FlowPolicy::default()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);
    assert!(matches!(checked.analysis.verdict, FlowVerdict::Denied));
    assert_eq!(checked.analysis.violations.len(), 1);
    assert!(checked.admit().is_err());
}

#[test]
fn inactive_policy_allows_unlabeled_flow() {
    let catalog = FlowCatalogView::default();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "q",
            "kind": "query",
            "qualified_entity": { "entry_id": "flow", "entity": "Message" },
            "expr": "Message",
            "ir": { "expr": { "op": "query", "entity": "Message" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "q" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");
    let topo = vec!["q".to_string()];
    let checked = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    assert!(matches!(checked.analysis.verdict, FlowVerdict::Clean));
    assert!(checked.admit().is_ok());
}

#[test]
fn bare_query_without_capability_name_uses_snake_case_fallback() {
    let mut catalog = FlowCatalogView::default();
    let read_key = QualifiedCapabilityKey::from_parts("github", "Issue", "issue_query");
    catalog.capability_output_labels.insert(
        read_key,
        BTreeSet::from([DataClassName::new("untrusted").expect("untrusted")]),
    );

    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "issues",
            "kind": "query",
            "qualified_entity": { "entry_id": "github", "entity": "Issue" },
            "expr": "Issue",
            "ir": { "expr": { "op": "query", "entity": "Issue" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "issues" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");
    let topo = vec!["issues".to_string()];
    let checked = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    let facts = checked
        .analysis
        .node_facts
        .get("issues")
        .expect("issues facts");
    assert!(
        facts
            .row_join()
            .labels
            .contains(&DataClassName::new("untrusted").expect("untrusted")),
        "snake_case fallback must resolve issue_query labels, got {:?}",
        facts.row_join().labels
    );
}

#[test]
fn entity_label_fallback_recovers_when_capability_key_misses() {
    let mut catalog = FlowCatalogView::default();
    let read_key = QualifiedCapabilityKey::from_parts("github", "Issue", "issue_query");
    catalog.capability_output_labels.insert(
        read_key,
        BTreeSet::from([DataClassName::new("untrusted").expect("untrusted")]),
    );

    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "issues",
            "kind": "query",
            "qualified_entity": { "entry_id": "github", "entity": "Issue" },
            "expr": "Issue",
            "ir": { "expr": { "op": "query", "entity": "Issue", "capability_name": "not_a_real_cap" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "issues" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");
    let topo = vec!["issues".to_string()];
    let checked = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    let facts = checked
        .analysis
        .node_facts
        .get("issues")
        .expect("issues facts");
    assert!(
        facts
            .row_join()
            .labels
            .contains(&DataClassName::new("untrusted").expect("untrusted")),
        "entity-level fallback must recover labels, got {:?}",
        facts.row_join().labels
    );
}

#[test]
fn approval_gate_json_shape_for_approve_enforcement() {
    let catalog = FlowCatalogView::default();
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
    let topo = vec!["c1".to_string()];

    let checked_inactive = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    assert!(
        checked_inactive
            .analysis
            .approval_gate_for_node("c1")
            .is_none(),
        "inactive policy must not produce approval gate"
    );

    let policy = FlowPolicy {
        default_posture: crate::plan_flow_policy::OperatorDisposition::Allow,
        capability_gates: vec![crate::plan_flow_policy::CapabilityGateRule {
            pattern: crate::plan_flow_policy::CapabilityGatePattern {
                entry_id: Some("acme".into()),
                entity: Some("Product".into()),
                capability: "create".into(),
            },
            enforcement: OperatorDisposition::Approve,
        }],
        ..FlowPolicy::default()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);
    let gate = checked
        .analysis
        .approval_gate_for_node("c1")
        .expect("approval gate for Approve enforcement");
    assert_eq!(gate["policy_key"], "acme.Product.create");
    assert_eq!(gate["host_policy"], "host.review");
    assert_eq!(gate["default_decision"], "approved");
    assert_eq!(gate["entry_id"], "acme");
    assert_eq!(gate["entity"], "Product");
    assert_eq!(gate["capability"], "create");
}
