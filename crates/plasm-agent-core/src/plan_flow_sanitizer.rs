//! Sanitizer clearance and robust-declassification for plan flow analysis.

use std::collections::{BTreeMap, BTreeSet};

use crate::plan_flow::{NodeFlowFacts, QualifiedCapabilityKey, SinkProof};
use crate::plan_flow_capability::resolve_alias_node;
use crate::plan_flow_ports::{FlowCatalog, FlowPolicyEvaluator};
use crate::plasm_plan::PlanResultUse;
use plasm_core::DataClassName;

pub struct LabelClearance {
    /// Labels removed by catalog/policy sanitizers (retained for inspection / future UI).
    #[allow(dead_code)]
    pub cleared: BTreeSet<DataClassName>,
    pub outgoing_labels: BTreeSet<DataClassName>,
    pub proof: SinkProof,
}

pub fn union_sanitizer_clears<C: FlowCatalog + ?Sized, P: FlowPolicyEvaluator + ?Sized>(
    catalog: &C,
    policy: &P,
    key: &QualifiedCapabilityKey,
    capability_name: &str,
) -> BTreeSet<DataClassName> {
    let mut cleared = catalog.sanitizers(key);
    cleared.extend(policy.policy_sanitizer_clears(capability_name));
    cleared
}

/// Apply catalog + policy sanitizer clearance to `incoming_labels`.
///
/// When `check_control_taint` is true, untrusted taint on behavior-controlling parameters
/// voids clearance (robust declassification); payload taint alone clears soundly.
#[allow(clippy::too_many_arguments)]
pub fn apply_label_clearance<C: FlowCatalog + ?Sized, P: FlowPolicyEvaluator + ?Sized>(
    catalog: &C,
    policy: &P,
    key: &QualifiedCapabilityKey,
    capability_name: &str,
    incoming_labels: BTreeSet<DataClassName>,
    template_expr: Option<&serde_json::Value>,
    uses_result: &[PlanResultUse],
    facts: &BTreeMap<String, NodeFlowFacts>,
    check_control_taint: bool,
) -> LabelClearance {
    let cleared = union_sanitizer_clears(catalog, policy, key, capability_name);
    if cleared.is_empty() {
        return LabelClearance {
            cleared,
            outgoing_labels: incoming_labels,
            proof: SinkProof::StaticClean,
        };
    }

    if check_control_taint {
        let control_params = catalog.control_params(key);
        let control_tainted = template_expr
            .map(|expr| control_param_untrusted_voided(expr, &control_params, uses_result, facts))
            .unwrap_or(false);
        if control_tainted {
            return LabelClearance {
                cleared,
                outgoing_labels: incoming_labels,
                proof: SinkProof::Deferred {
                    check: "robust_declassification_control_taint".to_string(),
                },
            };
        }
    }

    let mut labels = incoming_labels;
    for label in &cleared {
        labels.remove(label);
    }
    LabelClearance {
        cleared: cleared.clone(),
        outgoing_labels: labels,
        proof: SinkProof::Sanitized {
            by: capability_name.to_string(),
            cleared,
        },
    }
}

/// Returns true if any control param receives **untrusted** (integrity) taint.
fn control_param_untrusted_voided(
    expr: &serde_json::Value,
    control_params: &BTreeSet<String>,
    uses_result: &[PlanResultUse],
    facts: &BTreeMap<String, NodeFlowFacts>,
) -> bool {
    if control_params.is_empty() {
        return false;
    }
    let Some(input_obj) = expr.get("input").and_then(|v| v.as_object()) else {
        return false;
    };
    let Ok(untrusted) = DataClassName::new("untrusted") else {
        return false;
    };
    for (param_name, param_value) in input_obj {
        if !control_params.contains(param_name.as_str()) {
            continue;
        }
        if hole_value_has_label(param_value, uses_result, facts, &untrusted) {
            return true;
        }
    }
    false
}

fn hole_value_has_label(
    value: &serde_json::Value,
    uses_result: &[PlanResultUse],
    facts: &BTreeMap<String, NodeFlowFacts>,
    label: &DataClassName,
) -> bool {
    if let Some(hole) = value.as_object().and_then(|o| o.get("__plasm_hole")) {
        if hole.get("kind").and_then(|v| v.as_str()) == Some("node_input") {
            let alias = hole
                .get("alias")
                .or_else(|| hole.get("node"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let path: Vec<String> = hole
                .get("path")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let Some(source_id) = resolve_alias_node(uses_result, alias) else {
                return false;
            };
            let source_facts = facts.get(source_id.as_str()).cloned().unwrap_or_default();
            return source_facts.at_path(&path).labels.contains(label);
        }
        return false;
    }
    match value {
        serde_json::Value::Object(map) => map
            .values()
            .any(|v| hole_value_has_label(v, uses_result, facts, label)),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|v| hole_value_has_label(v, uses_result, facts, label)),
        _ => false,
    }
}
