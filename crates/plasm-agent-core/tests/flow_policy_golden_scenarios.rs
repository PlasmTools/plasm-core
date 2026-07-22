//! Golden scenario harness for Plan Security flow policy.
//!
//! Scenario catalog: `docs/flow-policy-golden-scenarios.md`.
//! Spec: `docs/plan-flow-policy-spec.md`.
//!
//! Test organization:
//!   - G-P*: pure policy disposition tests (no plan IR)
//!   - G-I*: inactive snapshot invariants
//!   - G-A*: plan-based IFC / forbidden flow tests (parse_and_validate_plan_json)
//!   - G-B*: boundary / discrimination cases
//!   - G-F*: forbidden override tests

use std::collections::BTreeSet;
use std::path::Path;

use plasm_agent_core::plasm_plan::parse_and_validate_plan_json;
use plasm_agent_core::{
    verify_plan_flow, CapabilityGatePattern, CapabilityGateRule, EffectEvent, FlowCatalogView,
    FlowPolicy, FlowPolicySnapshot, FlowVerdict, ForbiddenFlowRule, NodeDisposition,
    OperatorDisposition, PolicyRevision, QualifiedCapabilityKey, SanitizerRecognition,
    SinkParamRef,
};
use plasm_core::{load_schema_dir_unvalidated, CapabilityParamName, DataClassName, SinkClassName};

/// Workspace-root-relative path to the flow_matrix fixture.
fn flow_matrix_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/flow_matrix")
}

/// Build a minimal `FlowCatalogView` from the flow_matrix fixture CGS.
fn flow_matrix_view() -> FlowCatalogView {
    let dir = flow_matrix_dir();
    let cgs = load_schema_dir_unvalidated(&dir).expect("load flow_matrix fixture");
    FlowCatalogView::from_cgs("flow", &cgs)
}

/// Plan: query messages then send with body hole (taint flows: untrusted body → outbound_body sink).
fn query_then_send_plan() -> serde_json::Value {
    serde_json::json!({
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
                            "body": {
                                "__plasm_hole": {
                                    "kind": "node_input",
                                    "alias": "messages",
                                    "path": ["body"]
                                }
                            }
                        }
                    }
                }
            }
        ],
        "return": { "kind": "node", "node": "send" }
    })
}

/// Plan topo order for the query-then-send plan.
fn query_send_topo() -> Vec<String> {
    vec!["messages".to_string(), "send".to_string()]
}

/// Plan: query messages then sanitize_body with body hole (catalog clears untrusted).
fn query_then_sanitize_plan() -> serde_json::Value {
    serde_json::json!({
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
                "id": "sanitize",
                "kind": "action",
                "qualified_entity": { "entry_id": "flow", "entity": "Message" },
                "depends_on": ["messages"],
                "uses_result": [{ "node": "messages", "as": "messages" }],
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "ir_template": {
                    "expr": {
                        "op": "invoke",
                        "capability": "sanitize_body",
                        "target": { "entity_type": "Message", "key": { "id": "1" } },
                        "input": {
                            "body": {
                                "__plasm_hole": {
                                    "kind": "node_input",
                                    "alias": "messages",
                                    "path": ["body"]
                                }
                            }
                        }
                    }
                }
            }
        ],
        "return": { "kind": "node", "node": "sanitize" }
    })
}

fn query_sanitize_topo() -> Vec<String> {
    vec!["messages".to_string(), "sanitize".to_string()]
}

/// Hand-built FlowCatalogView for the query→send tests (avoids loading the full CGS for pure policy tests).
fn hand_built_catalog_with_untrusted_and_sink() -> FlowCatalogView {
    let mut catalog = FlowCatalogView::default();
    let read_key = QualifiedCapabilityKey::from_parts("flow", "Message", "message_query");
    let send_key = QualifiedCapabilityKey::from_parts("flow", "Message", "send");
    catalog.capability_output_labels.insert(
        read_key,
        BTreeSet::from([DataClassName::new("untrusted").expect("untrusted label")]),
    );
    catalog.capability_sink_params.insert(
        send_key,
        vec![SinkParamRef {
            param: CapabilityParamName::from("body"),
            sink_class: Some(SinkClassName::new("outbound_body").expect("outbound_body sink")),
        }],
    );
    catalog
}

// ─────────────────────────────────────────────────────────
// G-P: Pure policy disposition tests
// ─────────────────────────────────────────────────────────

/// G-P0: Inactive snapshot always Allows any mutation event.
#[test]
fn g_p0_inactive_snapshot_always_allows() {
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };
    let policy = FlowPolicySnapshot::Inactive.effective_policy();
    assert!(
        matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Allow
        ),
        "G-P0: Inactive snapshot must Allow any mutation"
    );
}

/// G-P1: Active snapshot, allow posture, no gates → Always Allow.
#[test]
fn g_p1_allow_posture_no_gates_always_allows() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };
    let effective = snapshot.effective_policy();
    assert!(
        matches!(
            effective.disposition_for_event(&event, None),
            NodeDisposition::Allow
        ),
        "G-P1: Active allow-posture with no gates must Allow"
    );
}

/// G-P2: Active snapshot, deny posture, no gates → Always Deny.
#[test]
fn g_p2_deny_posture_no_gates_always_blocks() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Deny,
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };
    let effective = snapshot.effective_policy();
    assert!(
        matches!(
            effective.disposition_for_event(&event, None),
            NodeDisposition::Deny
        ),
        "G-P2: Active deny-posture with no gates must Deny"
    );
}

/// G-P2b: Active snapshot, approve posture, no gates → Always Approve (HITL).
#[test]
fn g_p2b_approve_posture_no_gates_always_approves() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Approve,
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };
    let effective = snapshot.effective_policy();
    assert!(
        matches!(
            effective.disposition_for_event(&event, None),
            NodeDisposition::Approve { .. }
        ),
        "G-P2b: Active approve-posture with no gates must Approve"
    );
}

/// G-P3: deny posture + gate Allow on a specific capability → Allow for that cap, Deny for others.
#[test]
fn g_p3_deny_posture_gate_allow_carves_out() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Deny,
        capability_gates: vec![CapabilityGateRule {
            pattern: CapabilityGatePattern {
                entry_id: None,
                entity: None,
                capability: "comment_create".into(),
            },
            enforcement: OperatorDisposition::Allow,
        }],
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let effective = snapshot.effective_policy();

    let allowed_event = EffectEvent {
        entry_id: "linear".into(),
        entity: "Comment".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Create,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "comment_create".into(),
    };
    let blocked_event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };

    assert!(
        matches!(
            effective.disposition_for_event(&allowed_event, None),
            NodeDisposition::Allow
        ),
        "G-P3: deny-posture Allow gate must Allow the gated capability"
    );
    assert!(
        matches!(
            effective.disposition_for_event(&blocked_event, None),
            NodeDisposition::Deny
        ),
        "G-P3: deny-posture must Deny ungated capabilities"
    );
}

/// G-P4: allow posture + gate Deny on a capability → Deny for that cap, Allow for others.
#[test]
fn g_p4_allow_posture_gate_block_hard_stops() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        capability_gates: vec![CapabilityGateRule {
            pattern: CapabilityGatePattern {
                entry_id: Some("vultr".into()),
                entity: Some("KubernetesCluster".into()),
                capability: "delete_with_linked_resources".into(),
            },
            enforcement: OperatorDisposition::Deny,
        }],
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let effective = snapshot.effective_policy();

    let blocked_event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "KubernetesCluster".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete_with_linked_resources".into(),
    };
    let allowed_event = EffectEvent {
        entry_id: "linear".into(),
        entity: "Issue".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Create,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "issue_create".into(),
    };

    assert!(
        matches!(
            effective.disposition_for_event(&blocked_event, None),
            NodeDisposition::Deny
        ),
        "G-P4: Allow-posture Deny gate must Deny the gated capability"
    );
    assert!(
        matches!(
            effective.disposition_for_event(&allowed_event, None),
            NodeDisposition::Allow
        ),
        "G-P4: Allow-posture must Allow ungated capabilities"
    );
}

// ─────────────────────────────────────────────────────────
// G-I: Inactive snapshot invariants
// ─────────────────────────────────────────────────────────

/// G-I1: Inactive snapshot does NOT enforce forbidden rules (IFC disabled).
/// Even if labeled data would match a forbidden rule, Inactive = Allow on both layers.
#[test]
fn g_i1_inactive_does_not_enforce_forbidden() {
    let catalog = hand_built_catalog_with_untrusted_and_sink();
    let plan_json = query_then_send_plan();
    let validated = parse_and_validate_plan_json(&plan_json).expect("validate plan");
    let topo = query_send_topo();

    // This forbidden rule would deny under Active, but Inactive ignores it.
    let _would_deny_if_active = ForbiddenFlowRule {
        from_label: DataClassName::new("untrusted").expect("untrusted"),
        to_sink: Some(SinkClassName::new("outbound_body").expect("outbound_body")),
        reason: Some("not enforced when Inactive".into()),
    };

    let snapshot = FlowPolicySnapshot::Inactive;
    let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);

    assert!(
        matches!(checked.analysis.verdict, FlowVerdict::Clean),
        "G-I1: Inactive snapshot must not enforce forbidden rules; got {:?}",
        checked.analysis.verdict
    );
    assert!(
        checked.admit().is_ok(),
        "G-I1: Inactive snapshot must admit the plan"
    );
}

// ─────────────────────────────────────────────────────────
// G-A: Plan-based IFC / forbidden flow tests
// ─────────────────────────────────────────────────────────

/// G-A1: untrusted label on read output flows to send sink → Denied under Active + forbidden.
#[test]
fn g_a1_untrusted_read_to_sink_denied_by_forbidden() {
    let catalog = hand_built_catalog_with_untrusted_and_sink();
    let plan_json = query_then_send_plan();
    let validated = parse_and_validate_plan_json(&plan_json).expect("validate plan");
    let topo = query_send_topo();

    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        forbidden: vec![ForbiddenFlowRule {
            from_label: DataClassName::new("untrusted").expect("untrusted"),
            to_sink: Some(SinkClassName::new("outbound_body").expect("outbound_body")),
            reason: Some("untrusted must not reach outbound body".into()),
        }],
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };

    let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);
    assert!(
        matches!(checked.analysis.verdict, FlowVerdict::Denied),
        "G-A1: forbidden label→sink flow must Deny; got {:?}",
        checked.analysis.verdict
    );
    assert_eq!(
        checked.analysis.violations.len(),
        1,
        "G-A1: exactly one violation expected"
    );
    assert!(
        checked.admit().is_err(),
        "G-A1: Denied plan must not produce FlowAdmission"
    );
}

/// G-A4: Inactive snapshot + labeled flow → Clean (IFC not enforced).
#[test]
fn g_a4_inactive_labeled_flow_is_clean() {
    let catalog = hand_built_catalog_with_untrusted_and_sink();
    let plan_json = query_then_send_plan();
    let validated = parse_and_validate_plan_json(&plan_json).expect("validate plan");
    let topo = query_send_topo();

    let checked = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    assert!(
        matches!(checked.analysis.verdict, FlowVerdict::Clean),
        "G-A4: Inactive must produce Clean even with labeled flow; got {:?}",
        checked.analysis.verdict
    );
    assert!(checked.admit().is_ok(), "G-A4: must admit under Inactive");
}

/// G-A5: Control-arg untrusted taint voids robust-declassification clearance.
/// Payload taint alone clears soundly; taint on `keep_patterns` must defer clearance.
#[test]
fn g_a5_control_param_taint_voids_sanitizer_clearance() {
    let catalog = flow_matrix_view();
    let plan_json = serde_json::json!({
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
                "id": "redact",
                "kind": "action",
                "qualified_entity": { "entry_id": "flow", "entity": "Redactor" },
                "depends_on": ["messages"],
                "uses_result": [{ "node": "messages", "as": "messages" }],
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "ir_template": {
                    "expr": {
                        "op": "invoke",
                        "capability": "redact",
                        "target": { "entity_type": "Redactor", "key": { "id": "r1" } },
                        "input": {
                            "payload": {
                                "__plasm_hole": {
                                    "kind": "node_input",
                                    "alias": "messages",
                                    "path": ["body"]
                                }
                            },
                            "keep_patterns": {
                                "__plasm_hole": {
                                    "kind": "node_input",
                                    "alias": "messages",
                                    "path": ["body"]
                                }
                            }
                        }
                    }
                }
            }
        ],
        "return": { "kind": "node", "node": "redact" }
    });
    let validated = parse_and_validate_plan_json(&plan_json).expect("validate plan");
    let topo = vec!["messages".to_string(), "redact".to_string()];
    let checked = verify_plan_flow(
        validated.artifact(),
        &topo,
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    let proof = checked
        .analysis
        .sink_proofs
        .get("redact")
        .expect("redact sink proof");
    assert!(
        matches!(
            proof,
            plasm_agent_core::SinkProof::Deferred { check } if check == "robust_declassification_control_taint"
        ),
        "G-A5: control-arg untrusted taint must void clearance; got {proof:?}"
    );
    let facts = checked
        .analysis
        .node_facts
        .get("redact")
        .expect("redact facts");
    assert!(
        facts
            .row_join()
            .labels
            .contains(&DataClassName::new("untrusted").expect("untrusted")),
        "G-A5: voided clearance must preserve untrusted labels on output"
    );
}

// ─────────────────────────────────────────────────────────
// G-B: Boundary / discrimination cases
// ─────────────────────────────────────────────────────────

/// G-B4: Two capabilities on same entity, different dispositions.
/// `delete` → Approve; `delete_with_linked_resources` → Deny.
/// Capability name is the primary match key — they are independently gated.
#[test]
fn g_b4_same_entity_different_capability_dispositions() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        capability_gates: vec![
            CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: Some("vultr".into()),
                    entity: Some("KubernetesCluster".into()),
                    capability: "delete_with_linked_resources".into(),
                },
                enforcement: OperatorDisposition::Deny,
            },
            CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: Some("vultr".into()),
                    entity: Some("KubernetesCluster".into()),
                    capability: "delete".into(),
                },
                enforcement: OperatorDisposition::Approve,
            },
        ],
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };
    let effective = snapshot.effective_policy();

    let catastrophic = EffectEvent {
        entry_id: "vultr".into(),
        entity: "KubernetesCluster".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete_with_linked_resources".into(),
    };
    let plain = EffectEvent {
        capability: "delete".into(),
        ..catastrophic.clone()
    };

    assert!(
        matches!(
            effective.disposition_for_event(&catastrophic, None),
            NodeDisposition::Deny
        ),
        "G-B4: delete_with_linked_resources must Deny (catastrophic)"
    );
    assert!(
        matches!(
            effective.disposition_for_event(&plain, None),
            NodeDisposition::Approve { .. }
        ),
        "G-B4: delete must Approve (HITL, not catastrophic)"
    );
}

// ─────────────────────────────────────────────────────────
// G-F: Forbidden override tests
// ─────────────────────────────────────────────────────────

/// G-F1: Forbidden overrides gate Allow — IFC beats AC.
/// The capability_gate says Allow for `send`, but forbidden says untrusted → outbound_body.
/// Result: Denied. An Allow gate cannot suppress a matching forbidden rule.
#[test]
fn g_f1_forbidden_overrides_allow_gate() {
    let catalog = hand_built_catalog_with_untrusted_and_sink();
    let plan_json = query_then_send_plan();
    let validated = parse_and_validate_plan_json(&plan_json).expect("validate plan");
    let topo = query_send_topo();

    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        capability_gates: vec![CapabilityGateRule {
            // Explicitly allow `send` via AC gate — this does NOT suppress IFC.
            pattern: CapabilityGatePattern {
                entry_id: Some("flow".into()),
                entity: Some("Message".into()),
                capability: "send".into(),
            },
            enforcement: OperatorDisposition::Allow,
        }],
        forbidden: vec![ForbiddenFlowRule {
            from_label: DataClassName::new("untrusted").expect("untrusted"),
            to_sink: Some(SinkClassName::new("outbound_body").expect("outbound_body")),
            reason: Some("IFC must override AC Allow for label→sink flow".into()),
        }],
        sanitizers: vec![],
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };

    let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);
    assert!(
        matches!(checked.analysis.verdict, FlowVerdict::Denied),
        "G-F1: forbidden must override Allow gate; IFC beats AC; got {:?}",
        checked.analysis.verdict
    );
    assert_eq!(
        checked.analysis.violations.len(),
        1,
        "G-F1: exactly one IFC violation expected"
    );
    assert!(
        checked.admit().is_err(),
        "G-F1: Denied plan must not produce FlowAdmission"
    );
}

// ─────────────────────────────────────────────────────────
// Supplemental: flow_matrix fixture catalog coverage
// ─────────────────────────────────────────────────────────

/// Verify flow_matrix fixture has expected data classes and sink.
/// This guards the fixture from inadvertent deletion of required labels.
#[test]
fn flow_matrix_fixture_has_expected_labels_and_sink() {
    let view = flow_matrix_view();

    let send_key = QualifiedCapabilityKey::from_parts("flow", "Message", "send");
    let sink_params = view.sink_params_for(&send_key);
    assert!(
        sink_params.iter().any(|p| p
            .sink_class
            .as_ref()
            .is_some_and(|s| s.as_str() == "outbound_body")),
        "flow_matrix: send capability must expose outbound_body sink param"
    );

    let read_key = QualifiedCapabilityKey::from_parts("flow", "Message", "message_query");
    let labels = view.output_labels_for(&read_key);
    assert!(
        labels.iter().any(|l| l.as_str() == "untrusted"),
        "flow_matrix: message_query must carry untrusted label from Message.body; got {labels:?}"
    );
}

/// Verify flow_matrix fixture `redact` sanitizer clears `credentials` via catalog.
#[test]
fn flow_matrix_redact_sanitizes_credentials() {
    let view = flow_matrix_view();
    let redact_key = QualifiedCapabilityKey::from_parts("flow", "Redactor", "redact");
    let sanitizers = view.sanitizers_for(&redact_key);
    assert!(
        sanitizers.iter().any(|s| s.as_str() == "credentials"),
        "flow_matrix: redact capability must sanitize credentials; got {sanitizers:?}"
    );
}

/// Verify flow_matrix fixture `sanitize_body` sanitizer clears `untrusted` via catalog.
#[test]
fn flow_matrix_sanitize_body_clears_untrusted() {
    let view = flow_matrix_view();
    let key = QualifiedCapabilityKey::from_parts("flow", "Message", "sanitize_body");
    let sanitizers = view.sanitizers_for(&key);
    assert!(
        sanitizers.iter().any(|s| s.as_str() == "untrusted"),
        "flow_matrix: sanitize_body capability must sanitize untrusted; got {sanitizers:?}"
    );
}

// ─────────────────────────────────────────────────────────
// G-C: Catalog + policy sanitizer clearance
// G-C3 is covered by G-A5 (control-taint voids clearance) — no alias test.
// ─────────────────────────────────────────────────────────

/// G-C2: `sanitize_body` clears `untrusted` via catalog `sanitizes:` when control params are clean.
#[test]
fn g_c2_sanitize_body_clears_untrusted_via_catalog() {
    let catalog = flow_matrix_view();
    let validated = parse_and_validate_plan_json(&query_then_sanitize_plan()).expect("validate plan");
    let checked = verify_plan_flow(
        validated.artifact(),
        &query_sanitize_topo(),
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    let proof = checked
        .analysis
        .sink_proofs
        .get("sanitize")
        .expect("sanitize sink proof");
    assert!(
        matches!(
            proof,
            plasm_agent_core::SinkProof::Sanitized { by, cleared }
                if by == "sanitize_body"
                    && cleared.iter().any(|l| l.as_str() == "untrusted")
        ),
        "G-C2: sanitize_body must Sanitized-clear untrusted; got {proof:?}"
    );
    let facts = checked
        .analysis
        .node_facts
        .get("sanitize")
        .expect("sanitize facts");
    assert!(
        !facts
            .row_join()
            .labels
            .contains(&DataClassName::new("untrusted").expect("untrusted")),
        "G-C2: outgoing labels must not carry untrusted after sanitize_body"
    );
}

/// G-C4: Policy `sanitizers[]` augments catalog — clears credentials beyond catalog `sanitizes:`.
#[test]
fn g_c4_policy_sanitizer_augments_catalog_clearance() {
    let mut catalog = flow_matrix_view();
    let read_key = QualifiedCapabilityKey::from_parts("flow", "Message", "message_query");
    catalog
        .capability_output_labels
        .entry(read_key)
        .or_default()
        .insert(DataClassName::new("credentials").expect("credentials"));

    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        sanitizers: vec![SanitizerRecognition {
            capability: "sanitize_body".into(),
            clears: BTreeSet::from([DataClassName::new("credentials").expect("credentials")]),
        }],
        ..FlowPolicy::empty_allow()
    };
    let snapshot = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    };

    let validated = parse_and_validate_plan_json(&query_then_sanitize_plan()).expect("validate plan");
    let checked = verify_plan_flow(
        validated.artifact(),
        &query_sanitize_topo(),
        &catalog,
        &snapshot,
    );
    let proof = checked
        .analysis
        .sink_proofs
        .get("sanitize")
        .expect("sanitize sink proof");
    assert!(
        matches!(
            proof,
            plasm_agent_core::SinkProof::Sanitized { cleared, .. }
                if cleared.iter().any(|l| l.as_str() == "credentials")
                    && cleared.iter().any(|l| l.as_str() == "untrusted")
        ),
        "G-C4: policy+catalog clearance must include credentials and untrusted; got {proof:?}"
    );
    let facts = checked
        .analysis
        .node_facts
        .get("sanitize")
        .expect("sanitize facts");
    assert!(
        !facts
            .row_join()
            .labels
            .contains(&DataClassName::new("credentials").expect("credentials")),
        "G-C4: credentials must be cleared by policy sanitizer"
    );
}

// ─────────────────────────────────────────────────────────
// G-X / G-B2: Federated entry_id gate scoping
// G-B2 is the same contract as G-X1 — covered here, documented in the golden catalog.
// ─────────────────────────────────────────────────────────

/// G-X1 / G-B2: Gate scoped to `vultr`/`Instance`/`delete` blocks Vultr only.
#[test]
fn g_x1_entry_id_scoped_gate_does_not_block_other_catalog() {
    let policy = FlowPolicy {
        default_posture: OperatorDisposition::Allow,
        capability_gates: vec![CapabilityGateRule {
            pattern: CapabilityGatePattern {
                entry_id: Some("vultr".into()),
                entity: Some("Instance".into()),
                capability: "delete".into(),
            },
            enforcement: OperatorDisposition::Deny,
        }],
        ..FlowPolicy::empty_allow()
    };
    let effective = FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    }
    .effective_policy();

    let vultr_delete = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };
    let linear_delete = EffectEvent {
        entry_id: "linear".into(),
        entity: "Issue".into(),
        kind: plasm_agent_core::plasm_plan::PlanNodeKind::Delete,
        effect_class: plasm_agent_core::plasm_plan::EffectClass::Write,
        capability: "delete".into(),
    };

    assert!(
        matches!(
            effective.disposition_for_event(&vultr_delete, None),
            NodeDisposition::Deny
        ),
        "G-X1: vultr Instance delete must Deny"
    );
    assert!(
        matches!(
            effective.disposition_for_event(&linear_delete, None),
            NodeDisposition::Allow
        ),
        "G-X1: linear Issue delete must remain Allow (unmatched gate)"
    );
}
