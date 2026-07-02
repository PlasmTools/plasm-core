//! Canonical approval-gate JSON minted from flow dispositions only.

use crate::plan_flow::{ApprovalRequirement, NodeDisposition};
use crate::plasm_plan::{PlanNodeKind, QualifiedEntityKey};

/// Wire JSON shape for graph_summary `approval_gates` and per-node `approval_gate`.
pub fn approval_gate_json(requirement: &ApprovalRequirement, node_id: &str) -> serde_json::Value {
    serde_json::json!({
        "node": node_id,
        "required": true,
        "host_policy": match requirement.policy {
            crate::plan_flow_policy::ApprovalHostPolicy::AutoApprove => "host.auto_approve",
            crate::plan_flow_policy::ApprovalHostPolicy::RequireReview => "host.review",
        },
        "default_decision": "approved",
        "policy_key": requirement.policy_key,
        "entry_id": requirement.entry_id,
        "entity": requirement.entity,
        "operation": requirement.operation,
        "author_label": requirement.author_label,
        "reason": requirement.reason,
    })
}

pub fn approval_gate_from_disposition(
    node_id: &str,
    disposition: &NodeDisposition,
) -> Option<serde_json::Value> {
    match disposition {
        NodeDisposition::Approve { requirement } => Some(approval_gate_json(requirement, node_id)),
        _ => None,
    }
}

/// Action operations prefer the surface/template method name (`Product(...).label`).
pub fn action_name_from_template(expr_template: &str) -> Option<String> {
    let after_ref = expr_template.split(").").nth(1)?;
    let name = after_ref
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub fn effect_operation_label(
    kind: PlanNodeKind,
    capability_name: &str,
    expr_template: Option<&str>,
) -> String {
    if kind == PlanNodeKind::Action {
        if let Some(template) = expr_template {
            if let Some(name) = action_name_from_template(template) {
                return name;
            }
        }
        if capability_name != "action" {
            return capability_name.to_string();
        }
    }
    operation_name_for_kind(kind).to_string()
}

pub fn policy_key_for(q: &QualifiedEntityKey, operation: &str) -> String {
    format!(
        "{}.{}.{}",
        q.entry_id.as_str(),
        q.entity.as_str(),
        operation
    )
}

pub fn operation_name_for_kind(kind: PlanNodeKind) -> &'static str {
    match kind {
        PlanNodeKind::Create => "create",
        PlanNodeKind::Update => "update",
        PlanNodeKind::Delete => "delete",
        PlanNodeKind::Action => "action",
        PlanNodeKind::Query => "query",
        PlanNodeKind::Search => "search",
        PlanNodeKind::Get => "get",
        PlanNodeKind::Data => "data",
        PlanNodeKind::Derive => "derive",
        PlanNodeKind::Compute => "compute",
        PlanNodeKind::ForEach => "for_each",
        PlanNodeKind::Relation => "relation",
    }
}
