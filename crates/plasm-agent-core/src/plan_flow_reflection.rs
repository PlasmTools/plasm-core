//! Data-flow projection for the plan MCP UI's Flow tab — mandatory, sibling to
//! [`crate::plan_ux_reflection`]. Renders [`crate::plan_flow::PlanFlowAnalysis`] as a UI-ready
//! trace: per-step labels, dispositions, and policy violations, independent of execution.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::execute_session::ExecuteSession;
use crate::plan_flow::{FlowVerdict, NodeDisposition, PlanFlowAnalysis};
use crate::plan_flow_ports::FlowCatalog;
use crate::plan_ux_reflection::PlanUxStep;
use crate::plasm_plan::{Plan, ValidatedPlanState};
use crate::plasm_plan_run::node_dependencies;
use plasm_core::DataClassSeverity;

pub const PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUxFlowVerdict {
    Clean,
    NeedsReview,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUxFlowDisposition {
    Allow,
    Approve,
    Review,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanUxFlowCounts {
    pub allow: usize,
    pub approve: usize,
    pub review: usize,
    pub deny: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxFlowSink {
    pub param: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxFlowViolation {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<PlanUxFlowSink>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxFlowApproval {
    pub operation: String,
    pub policy_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxFlowStep {
    pub id: String,
    pub ordinal: u8,
    pub disposition: PlanUxFlowDisposition,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_in: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_out: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<PlanUxFlowSink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<PlanUxFlowApproval>,
}

/// Severity + description for a data-class label referenced by the flow trace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxDataClassInfo {
    pub severity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxFlowReflection {
    pub schema_version: u32,
    pub verdict: PlanUxFlowVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<u64>,
    pub counts: PlanUxFlowCounts,
    /// True when any pinned catalog capability produces labeled output.
    /// Distinguishes "catalog has no labels" from "this plan touched no labeled data".
    pub catalog_has_labels: bool,
    /// Severity/description for every label name referenced in `trace` / `violations`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data_classes: BTreeMap<String, PlanUxDataClassInfo>,
    #[serde(default)]
    pub violations: Vec<PlanUxFlowViolation>,
    #[serde(default)]
    pub trace: Vec<PlanUxFlowStep>,
}

fn flow_verdict_wire(verdict: &FlowVerdict) -> PlanUxFlowVerdict {
    match verdict {
        FlowVerdict::Clean => PlanUxFlowVerdict::Clean,
        FlowVerdict::NeedsReview => PlanUxFlowVerdict::NeedsReview,
        FlowVerdict::Denied => PlanUxFlowVerdict::Denied,
    }
}

/// Disposition + its approval payload from a single lookup — the two are intrinsically linked
/// (only `Approve` carries a requirement), so one mapping function derives both facts at once
/// rather than matching `node_dispositions` twice per step.
fn disposition_and_approval(
    disposition: Option<&NodeDisposition>,
) -> (PlanUxFlowDisposition, Option<PlanUxFlowApproval>) {
    match disposition {
        Some(NodeDisposition::Approve { requirement }) => (
            PlanUxFlowDisposition::Approve,
            Some(PlanUxFlowApproval {
                operation: requirement.operation.clone(),
                policy_key: requirement.policy_key.clone(),
                reason: requirement.reason.clone(),
            }),
        ),
        Some(NodeDisposition::Review) => (PlanUxFlowDisposition::Review, None),
        Some(NodeDisposition::Deny) => (PlanUxFlowDisposition::Deny, None),
        Some(NodeDisposition::Allow) | None => (PlanUxFlowDisposition::Allow, None),
    }
}

fn sink_wire(sink: &crate::plan_flow::SinkParamRef) -> PlanUxFlowSink {
    PlanUxFlowSink {
        param: sink.param.as_str().to_string(),
        sink_class: sink.sink_class.as_ref().map(|c| c.as_str().to_string()),
    }
}

/// A node's own output labels (its `NodeFlowFacts`, row-joined) — the single primitive both
/// `labels_out` (this node) and `labels_in` (union over dependency outputs) build on.
fn node_output_labels(analysis: &PlanFlowAnalysis, node_id: &str) -> BTreeSet<String> {
    analysis
        .node_facts
        .get(node_id)
        .map(|facts| facts.row_join().labels)
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.as_str().to_string())
        .collect()
}

fn node_labels_in(
    plan: &Plan<ValidatedPlanState>,
    analysis: &PlanFlowAnalysis,
    node_id: &str,
) -> Vec<String> {
    let Some(node) = plan.nodes.iter().find(|n| n.id().as_str() == node_id) else {
        return Vec::new();
    };
    let mut labels = BTreeSet::new();
    for dep_id in node_dependencies(node) {
        labels.extend(node_output_labels(analysis, &dep_id));
    }
    labels.into_iter().collect()
}

fn severity_wire(severity: DataClassSeverity) -> &'static str {
    match severity {
        DataClassSeverity::Info => "info",
        DataClassSeverity::Sensitive => "sensitive",
        DataClassSeverity::Untrusted => "untrusted",
        DataClassSeverity::Critical => "critical",
    }
}

/// Catalog-wide label presence + severity metadata for labels referenced by this plan.
fn catalog_label_meta(
    session: Option<&ExecuteSession>,
    referenced: &BTreeSet<String>,
) -> (bool, BTreeMap<String, PlanUxDataClassInfo>) {
    // Index every pinned catalog's data-class schemas once, then project only
    // the labels this plan actually references (defaults when session is absent).
    let mut by_name: BTreeMap<&str, (&'static str, &str)> = BTreeMap::new();
    let catalog_has_labels = session
        .map(|session| {
            for ctx in session.contexts_by_entry.values() {
                for (name, schema) in &ctx.cgs.data_classes {
                    by_name
                        .entry(name.as_str())
                        .or_insert((severity_wire(schema.severity), schema.description.as_str()));
                }
            }
            session.build_flow_catalog_view().has_any_output_labels() || !by_name.is_empty()
        })
        .unwrap_or(false);

    let data_classes = referenced
        .iter()
        .map(|name| {
            let info = by_name
                .get(name.as_str())
                .map(|(sev, desc)| PlanUxDataClassInfo {
                    severity: (*sev).to_string(),
                    description: (*desc).to_string(),
                })
                .unwrap_or_else(|| PlanUxDataClassInfo {
                    severity: "info".into(),
                    description: String::new(),
                });
            (name.clone(), info)
        })
        .collect();

    (catalog_has_labels, data_classes)
}

/// Project [`PlanFlowAnalysis`] into the Flow tab's wire shape. `steps` is the already-built
/// [`PlanUxStep`] list (shared ordinals/headlines with the Plan tab); this adds no execution
/// data — static analysis only, independent of the run explorer.
pub fn plan_ux_flow_reflection(
    plan: &Plan<ValidatedPlanState>,
    analysis: &PlanFlowAnalysis,
    steps: &[PlanUxStep],
    session: Option<&ExecuteSession>,
) -> PlanUxFlowReflection {
    let mut counts = PlanUxFlowCounts::default();
    for disposition in analysis.node_dispositions.values() {
        match disposition {
            NodeDisposition::Allow => counts.allow += 1,
            NodeDisposition::Approve { .. } => counts.approve += 1,
            NodeDisposition::Review => counts.review += 1,
            NodeDisposition::Deny => counts.deny += 1,
        }
    }

    let headline_by_id: BTreeMap<&str, Option<&str>> = steps
        .iter()
        .map(|s| (s.id.as_str(), s.headline.as_deref()))
        .collect();

    let violations: Vec<PlanUxFlowViolation> = analysis
        .violations
        .iter()
        .map(|v| PlanUxFlowViolation {
            node_id: v.node.clone(),
            headline: headline_by_id
                .get(v.node.as_str())
                .copied()
                .flatten()
                .map(str::to_string),
            reason: v.reason.clone(),
            labels: v.labels.iter().map(|l| l.as_str().to_string()).collect(),
            sink: v.sink_param.as_ref().map(sink_wire),
        })
        .collect();

    let sink_by_node: BTreeMap<&str, Option<PlanUxFlowSink>> = violations
        .iter()
        .map(|v| (v.node_id.as_str(), v.sink.clone()))
        .collect();

    let trace: Vec<PlanUxFlowStep> = steps
        .iter()
        .map(|step| {
            let (disposition, approval) =
                disposition_and_approval(analysis.node_dispositions.get(&step.id));
            PlanUxFlowStep {
                id: step.id.clone(),
                ordinal: step.ordinal,
                disposition,
                labels_in: node_labels_in(plan, analysis, &step.id),
                labels_out: node_output_labels(analysis, &step.id).into_iter().collect(),
                sink: sink_by_node.get(step.id.as_str()).cloned().flatten(),
                approval,
            }
        })
        .collect();

    let mut referenced = BTreeSet::new();
    for step in &trace {
        referenced.extend(step.labels_in.iter().cloned());
        referenced.extend(step.labels_out.iter().cloned());
    }
    for v in &violations {
        referenced.extend(v.labels.iter().cloned());
    }
    let (catalog_has_labels, data_classes) = catalog_label_meta(session, &referenced);

    PlanUxFlowReflection {
        schema_version: PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION,
        verdict: flow_verdict_wire(&analysis.verdict),
        policy_revision: analysis.policy_revision.map(|r| r.0),
        counts,
        catalog_has_labels,
        data_classes,
        violations,
        trace,
    }
}

/// Reject stale or partial `plan_ux_reflection.flow` wire (exact schema cutover).
pub fn validate_plan_ux_flow_reflection_wire(v: &serde_json::Value) -> Result<(), String> {
    let flow: PlanUxFlowReflection = serde_json::from_value(v.clone())
        .map_err(|e| format!("plan_ux_reflection.flow invalid: {e}"))?;
    if flow.schema_version != PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION {
        return Err(format!(
            "plan_ux_reflection.flow.schema_version must be {} (got {})",
            PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION, flow.schema_version
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_catalog::FlowCatalogView;
    use crate::plan_flow::{verify_plan_flow, QualifiedCapabilityKey, SinkParamRef};
    use crate::plan_flow_policy::{
        FlowPolicy, FlowPolicySnapshot, ForbiddenFlowRule, PolicyRevision,
    };
    use crate::plasm_plan::parse_and_validate_plan_json;
    use plasm_core::{CapabilityParamName, DataClassName, SinkClassName};

    fn deny_plan_analysis() -> (Plan<ValidatedPlanState>, PlanFlowAnalysis) {
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

        let mut catalog = FlowCatalogView::default();
        let read_key = QualifiedCapabilityKey::from_parts("flow", "Message", "Message_query");
        let send_key = QualifiedCapabilityKey::from_parts("flow", "Message", "send");
        catalog.capability_output_labels.insert(
            read_key,
            std::collections::BTreeSet::from([DataClassName::new("untrusted").expect("label")]),
        );
        catalog.capability_sink_params.insert(
            send_key,
            vec![SinkParamRef {
                param: CapabilityParamName::from("body"),
                sink_class: Some(SinkClassName::new("outbound_body").expect("sink")),
            }],
        );
        let topo = vec!["messages".to_string(), "send".to_string()];
        let policy = FlowPolicy {
            forbidden: vec![ForbiddenFlowRule {
                from_label: DataClassName::new("untrusted").expect("label"),
                to_sink: Some(SinkClassName::new("outbound_body").expect("sink")),
                reason: Some("untrusted cannot reach outbound body".into()),
            }],
            ..FlowPolicy::default()
        };
        let snapshot = FlowPolicySnapshot::Active {
            revision: PolicyRevision(7),
            policy,
        };
        let checked = verify_plan_flow(validated.artifact(), &topo, &catalog, &snapshot);
        (validated.artifact().clone(), checked.analysis)
    }

    fn step(id: &str, ordinal: u8) -> PlanUxStep {
        PlanUxStep {
            id: id.to_string(),
            ordinal,
            widget: crate::plan_ux_reflection::PlanUxWidgetKind::ReadSurface,
            entry_id: None,
            entity: None,
            qualified_entity: None,
            operation: id.to_string(),
            effect_class: "read".into(),
            approval_gate: false,
            layout_hint: None,
            headline: None,
        }
    }

    #[test]
    fn flow_reflection_surfaces_denied_sink_violation() {
        let (plan, analysis) = deny_plan_analysis();
        let steps = vec![step("messages", 1), step("send", 2)];
        let flow = plan_ux_flow_reflection(&plan, &analysis, &steps, None);

        assert_eq!(flow.verdict, PlanUxFlowVerdict::Denied);
        assert!(!flow.catalog_has_labels);
        assert!(flow.data_classes.contains_key("untrusted"));
        assert_eq!(
            flow.data_classes["untrusted"].severity, "info",
            "without session, severity defaults to info"
        );
        assert_eq!(flow.counts.deny, 1);
        assert_eq!(flow.policy_revision, Some(7));
        assert_eq!(flow.violations.len(), 1);
        let violation = &flow.violations[0];
        assert_eq!(violation.node_id, "send");
        assert_eq!(violation.labels, vec!["untrusted".to_string()]);
        assert_eq!(
            violation.sink.as_ref().map(|s| s.param.as_str()),
            Some("body")
        );

        let send_step = flow
            .trace
            .iter()
            .find(|s| s.id == "send")
            .expect("send step");
        assert_eq!(send_step.disposition, PlanUxFlowDisposition::Deny);
        assert!(send_step.labels_in.contains(&"untrusted".to_string()));
        assert!(send_step.sink.is_some());

        let messages_step = flow
            .trace
            .iter()
            .find(|s| s.id == "messages")
            .expect("messages step");
        assert_eq!(messages_step.disposition, PlanUxFlowDisposition::Allow);
        assert!(messages_step.labels_out.contains(&"untrusted".to_string()));
        assert!(messages_step.labels_in.is_empty());
    }

    #[test]
    fn validate_flow_wire_accepts_missing_violations_and_trace() {
        let partial = serde_json::json!({
            "schema_version": PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION,
            "verdict": "clean",
            "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
            "catalog_has_labels": false
        });
        validate_plan_ux_flow_reflection_wire(&partial).expect("partial flow wire");
        let flow: PlanUxFlowReflection =
            serde_json::from_value(partial).expect("deserialize partial flow");
        assert!(flow.violations.is_empty());
        assert!(flow.trace.is_empty());
        assert!(!flow.catalog_has_labels);
    }

    #[test]
    fn validate_flow_wire_rejects_stale_schema_v1() {
        let stale = serde_json::json!({
            "schema_version": 1,
            "verdict": "clean",
            "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
            "catalog_has_labels": false
        });
        let err = validate_plan_ux_flow_reflection_wire(&stale).unwrap_err();
        assert!(err.contains("schema_version must be 2"), "{err}");
    }
}
