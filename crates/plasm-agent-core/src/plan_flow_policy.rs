//! Typed, host-owned plan flow policy snapshot.

use crate::approval_gate::{effect_operation_label, policy_key_for};
use crate::plan_flow::{ApprovalRequirement, NodeDisposition};
use crate::plasm_plan::{EffectClass, PlanNodeKind, QualifiedEntityKey};
use plasm_core::{DataClassName, SinkClassName};
use serde::Serialize;
use std::collections::BTreeSet;

/// Monotonic tenant-owned policy revision marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
pub struct PolicyRevision(pub u64);

/// Host policy used when a node requires approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalHostPolicy {
    AutoApprove,
    RequireReview,
}

/// Node-level flow enforcement selected by policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowEnforcement {
    Allow,
    Approve(ApprovalHostPolicy),
    Review,
    Deny,
}

impl FlowEnforcement {
    pub fn to_disposition(&self, requirement: ApprovalRequirement) -> NodeDisposition {
        match self {
            Self::Allow => NodeDisposition::Allow,
            Self::Approve(policy) => NodeDisposition::Approve {
                requirement: ApprovalRequirement {
                    policy: *policy,
                    ..requirement
                },
            },
            Self::Review => NodeDisposition::Review,
            Self::Deny => NodeDisposition::Deny,
        }
    }
}

/// Concrete effect event for policy rule matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectEvent {
    pub entry_id: String,
    pub entity: String,
    pub kind: PlanNodeKind,
    pub effect_class: EffectClass,
    pub operation: String,
}

impl EffectEvent {
    pub fn from_mutation(
        q: &QualifiedEntityKey,
        kind: PlanNodeKind,
        effect_class: EffectClass,
        capability_name: &str,
        expr_template: Option<&str>,
    ) -> Self {
        let operation = effect_operation_label(kind, capability_name, expr_template);
        Self {
            entry_id: q.entry_id.as_str().to_string(),
            entity: q.entity.as_str().to_string(),
            kind,
            effect_class,
            operation,
        }
    }

    pub fn approval_requirement(
        &self,
        policy: ApprovalHostPolicy,
        author_label: Option<&str>,
    ) -> ApprovalRequirement {
        ApprovalRequirement {
            policy,
            entry_id: self.entry_id.clone(),
            entity: self.entity.clone(),
            operation: self.operation.clone(),
            policy_key: policy_key_for(
                &QualifiedEntityKey {
                    entry_id: self.entry_id.clone(),
                    entity: self.entity.clone(),
                },
                &self.operation,
            ),
            author_label: author_label.map(str::to_string),
            reason: Some(format!(
                "mutating capability {:?} on {}.{}",
                self.kind, self.entry_id, self.entity
            )),
        }
    }
}

/// Pattern matched against effect events (surface or for_each effect template).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectEventPattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<PlanNodeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_class: Option<EffectClass>,
}

impl EffectEventPattern {
    pub fn remote_mutation_defaults() -> Vec<Self> {
        [
            PlanNodeKind::Create,
            PlanNodeKind::Update,
            PlanNodeKind::Delete,
            PlanNodeKind::Action,
        ]
        .into_iter()
        .map(|operation| Self {
            entry_id: None,
            entity: None,
            operation: Some(operation),
            effect_class: None,
        })
        .chain(
            [EffectClass::Write, EffectClass::SideEffect]
                .into_iter()
                .map(|effect_class| Self {
                    entry_id: None,
                    entity: None,
                    operation: None,
                    effect_class: Some(effect_class),
                }),
        )
        .collect()
    }

    pub fn matches(&self, event: &EffectEvent) -> bool {
        if self
            .entry_id
            .as_ref()
            .is_some_and(|entry| entry != &event.entry_id)
        {
            return false;
        }
        if self
            .entity
            .as_ref()
            .is_some_and(|entity| entity != &event.entity)
        {
            return false;
        }
        if self
            .operation
            .is_some_and(|operation| operation != event.kind)
        {
            return false;
        }
        if self
            .effect_class
            .is_some_and(|effect_class| effect_class != event.effect_class)
        {
            return false;
        }
        true
    }
}

/// Explicit policy deny-list for label -> sink flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForbiddenFlowRule {
    pub from_label: DataClassName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_sink: Option<SinkClassName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Effect-matching rule that sets default disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectRule {
    pub pattern: EffectEventPattern,
    pub enforcement: FlowEnforcement,
}

/// Capabilities recognized as sanitizers for selected labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SanitizerRecognition {
    pub capability: String,
    #[serde(default)]
    pub clears: BTreeSet<DataClassName>,
}

/// Policy payload pinned to an execute session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowPolicy {
    #[serde(default)]
    pub forbidden: Vec<ForbiddenFlowRule>,
    #[serde(default)]
    pub effect_rules: Vec<EffectRule>,
    #[serde(default)]
    pub sanitizers: Vec<SanitizerRecognition>,
}

impl FlowPolicy {
    pub fn default_remote_mutation_auto_approve() -> Self {
        Self {
            forbidden: Vec::new(),
            effect_rules: EffectEventPattern::remote_mutation_defaults()
                .into_iter()
                .map(|pattern| EffectRule {
                    pattern,
                    enforcement: FlowEnforcement::Approve(ApprovalHostPolicy::AutoApprove),
                })
                .collect(),
            sanitizers: Vec::new(),
        }
    }

    pub fn disposition_for_event(
        &self,
        event: &EffectEvent,
        author_label: Option<&str>,
    ) -> NodeDisposition {
        for rule in &self.effect_rules {
            if !rule.pattern.matches(event) {
                continue;
            }
            let requirement = match rule.enforcement {
                FlowEnforcement::Allow => {
                    return NodeDisposition::Allow;
                }
                FlowEnforcement::Approve(policy) => {
                    event.approval_requirement(policy, author_label)
                }
                FlowEnforcement::Review => {
                    return NodeDisposition::Review;
                }
                FlowEnforcement::Deny => {
                    return NodeDisposition::Deny;
                }
            };
            return rule.enforcement.to_disposition(requirement);
        }
        NodeDisposition::Allow
    }
}

impl Default for FlowPolicy {
    fn default() -> Self {
        Self::default_remote_mutation_auto_approve()
    }
}

/// Session-pinned snapshot arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FlowPolicySnapshot {
    /// Compatibility mode: still advertises default host auto-approve policy for
    /// mutating effects, but does not yet enforce stricter deny/review behavior.
    #[default]
    Inactive,
    Active {
        revision: PolicyRevision,
        policy: FlowPolicy,
    },
}

impl FlowPolicySnapshot {
    pub fn inactive_default() -> Self {
        Self::Inactive
    }

    pub fn effective_policy(&self) -> FlowPolicy {
        match self {
            Self::Inactive => FlowPolicy::default_remote_mutation_auto_approve(),
            Self::Active { policy, .. } => policy.clone(),
        }
    }

    pub fn revision_or_default(&self) -> PolicyRevision {
        match self {
            Self::Inactive => PolicyRevision::default(),
            Self::Active { revision, .. } => *revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_rules_match_remote_mutations() {
        let policy = FlowPolicy::default();
        let event = EffectEvent {
            entry_id: "acme".into(),
            entity: "Product".into(),
            kind: PlanNodeKind::Create,
            effect_class: EffectClass::Write,
            operation: "create".into(),
        };
        let disposition = policy.disposition_for_event(&event, None);
        assert!(matches!(disposition, NodeDisposition::Approve { .. }));
    }
}
