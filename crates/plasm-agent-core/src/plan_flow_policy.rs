//! Typed, host-owned plan flow policy snapshot.
//!
//! Clean cutover: one operator disposition vocabulary (`allow` | `approve` | `deny`)
//! for both `default_posture` and `capability_gates[].enforcement`.
//! `FlowPolicySnapshot::Inactive` = system Allow.

use crate::approval_gate::{effect_operation_label, policy_key_for};
use crate::plan_flow::{ApprovalRequirement, NodeDisposition};
use crate::plasm_plan::{EffectClass, PlanNodeKind, QualifiedEntityKey};
use plasm_core::{DataClassName, SinkClassName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Monotonic tenant-owned policy revision marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
pub struct PolicyRevision(pub u64);

/// Host policy used when a node requires human approval (HITL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalHostPolicy {
    RequireReview,
}

/// Authorable operator disposition — shared by posture and capability gates.
///
/// Wire: `"allow"` | `"approve"` | `"deny"`.
/// Structural plan review (boundedness) is engine-only — never an authorable rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDisposition {
    /// Mutation proceeds without HITL.
    #[default]
    Allow,
    /// Mutation requires human approval (HITL).
    Approve,
    /// Mutation is blocked; no commit token.
    Deny,
}

impl OperatorDisposition {
    /// Map to a flow-pass node disposition. HITL requirement is minted only for Approve.
    pub fn to_node_disposition(
        self,
        event: &EffectEvent,
        author_label: Option<&str>,
    ) -> NodeDisposition {
        match self {
            Self::Allow => NodeDisposition::Allow,
            Self::Deny => NodeDisposition::Deny,
            Self::Approve => NodeDisposition::Approve {
                requirement: event
                    .approval_requirement(ApprovalHostPolicy::RequireReview, author_label),
            },
        }
    }
}

/// Concrete capability event for policy gate matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectEvent {
    pub entry_id: String,
    pub entity: String,
    pub kind: PlanNodeKind,
    pub effect_class: EffectClass,
    /// Capability name (from IR / display) — primary match key for gates.
    pub capability: String,
}

impl EffectEvent {
    pub fn from_mutation(
        q: &QualifiedEntityKey,
        kind: PlanNodeKind,
        effect_class: EffectClass,
        capability_name: &str,
        expr_template: Option<&str>,
    ) -> Self {
        let capability = if !capability_name.is_empty() && capability_name != "action" {
            capability_name.to_string()
        } else {
            effect_operation_label(kind, capability_name, expr_template)
        };
        Self {
            entry_id: q.entry_id.as_str().to_string(),
            entity: q.entity.as_str().to_string(),
            kind,
            effect_class,
            capability,
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
            capability: self.capability.clone(),
            policy_key: policy_key_for(
                &QualifiedEntityKey {
                    entry_id: self.entry_id.clone(),
                    entity: self.entity.clone(),
                },
                &self.capability,
            ),
            author_label: author_label.map(str::to_string),
            reason: Some(format!(
                "mutating capability {:?} on {}.{}",
                self.kind, self.entry_id, self.entity
            )),
        }
    }
}

/// Pattern matched against capability events (surface or for_each effect template).
///
/// Struct (not tuple) so a future optional `authority` / `acts_for` field can be
/// added without a wire break (delegated-identity axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGatePattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Required: capability name matching `EffectEvent.capability`.
    pub capability: String,
}

impl CapabilityGatePattern {
    pub fn matches(&self, event: &EffectEvent) -> bool {
        if self.capability != event.capability {
            return false;
        }
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
        true
    }
}

/// Explicit policy deny-list for label -> sink flow (IFC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForbiddenFlowRule {
    pub from_label: DataClassName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_sink: Option<SinkClassName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Capability gate: access-control disposition when pattern matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGateRule {
    pub pattern: CapabilityGatePattern,
    pub enforcement: OperatorDisposition,
}

/// Capabilities recognized as sanitizers (declassification / endorsement).
///
/// Struct retained so a future optional `authority` field slots in without
/// breaking wire (delegated-identity axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizerRecognition {
    pub capability: String,
    #[serde(default)]
    pub clears: BTreeSet<DataClassName>,
}

/// Policy payload pinned to an execute session (Active ruleset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowPolicy {
    /// Required on published rulesets; serde-default for draft convenience.
    #[serde(default)]
    pub default_posture: OperatorDisposition,
    #[serde(default)]
    pub forbidden: Vec<ForbiddenFlowRule>,
    #[serde(default)]
    pub capability_gates: Vec<CapabilityGateRule>,
    #[serde(default)]
    pub sanitizers: Vec<SanitizerRecognition>,
}

impl FlowPolicy {
    /// Empty ruleset with Allow posture (no gates, no forbidden).
    pub fn empty_allow() -> Self {
        Self {
            default_posture: OperatorDisposition::Allow,
            forbidden: Vec::new(),
            capability_gates: Vec::new(),
            sanitizers: Vec::new(),
        }
    }

    pub fn disposition_for_event(
        &self,
        event: &EffectEvent,
        author_label: Option<&str>,
    ) -> NodeDisposition {
        for rule in &self.capability_gates {
            if !rule.pattern.matches(event) {
                continue;
            }
            return rule
                .enforcement
                .to_node_disposition(event, author_label);
        }
        self.default_posture
            .to_node_disposition(event, author_label)
    }

    /// Labels this policy recognizes as cleared by the given capability name.
    pub fn policy_sanitizer_clears(&self, capability: &str) -> BTreeSet<DataClassName> {
        self.sanitizers
            .iter()
            .filter(|s| s.capability == capability)
            .flat_map(|s| s.clears.iter().cloned())
            .collect()
    }
}

impl Default for FlowPolicy {
    fn default() -> Self {
        Self::empty_allow()
    }
}

/// Session-pinned snapshot arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FlowPolicySnapshot {
    /// No published ruleset: system default = Allow all mutations.
    /// IFC `forbidden[]` is not enforced until a policy is Active.
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
            Self::Inactive => FlowPolicy::empty_allow(),
            Self::Active { policy, .. } => policy.clone(),
        }
    }

    /// Whether IFC forbidden rules apply (only Active rulesets).
    pub fn enforces_forbidden(&self) -> bool {
        matches!(self, Self::Active { .. })
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
    fn inactive_and_empty_allow_unmatched_mutations() {
        let event = EffectEvent {
            entry_id: "acme".into(),
            entity: "Product".into(),
            kind: PlanNodeKind::Create,
            effect_class: EffectClass::Write,
            capability: "create".into(),
        };
        let inactive = FlowPolicySnapshot::Inactive.effective_policy();
        assert!(matches!(
            inactive.disposition_for_event(&event, None),
            NodeDisposition::Allow
        ));
        let empty = FlowPolicy::empty_allow();
        assert!(matches!(
            empty.disposition_for_event(&event, None),
            NodeDisposition::Allow
        ));
    }

    #[test]
    fn default_posture_deny_blocks_unmatched() {
        let policy = FlowPolicy {
            default_posture: OperatorDisposition::Deny,
            ..FlowPolicy::empty_allow()
        };
        let event = EffectEvent {
            entry_id: "vultr".into(),
            entity: "Instance".into(),
            kind: PlanNodeKind::Delete,
            effect_class: EffectClass::Write,
            capability: "delete".into(),
        };
        assert!(matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Deny
        ));
    }

    #[test]
    fn default_posture_approve_requires_hitl_for_unmatched() {
        let policy = FlowPolicy {
            default_posture: OperatorDisposition::Approve,
            ..FlowPolicy::empty_allow()
        };
        let event = EffectEvent {
            entry_id: "vultr".into(),
            entity: "Instance".into(),
            kind: PlanNodeKind::Delete,
            effect_class: EffectClass::Write,
            capability: "delete".into(),
        };
        assert!(matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Approve { .. }
        ));
    }

    #[test]
    fn capability_gate_matches_by_name() {
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
        let catastrophic = EffectEvent {
            entry_id: "vultr".into(),
            entity: "KubernetesCluster".into(),
            kind: PlanNodeKind::Delete,
            effect_class: EffectClass::Write,
            capability: "delete_with_linked_resources".into(),
        };
        let plain = EffectEvent {
            capability: "delete".into(),
            ..catastrophic.clone()
        };
        assert!(matches!(
            policy.disposition_for_event(&catastrophic, None),
            NodeDisposition::Deny
        ));
        assert!(matches!(
            policy.disposition_for_event(&plain, None),
            NodeDisposition::Approve { .. }
        ));
    }

    #[test]
    fn default_deny_gate_allow_whitelists() {
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
        let allowed = EffectEvent {
            entry_id: "linear".into(),
            entity: "Comment".into(),
            kind: PlanNodeKind::Create,
            effect_class: EffectClass::Write,
            capability: "comment_create".into(),
        };
        let blocked = EffectEvent {
            capability: "delete".into(),
            ..allowed.clone()
        };
        assert!(matches!(
            policy.disposition_for_event(&allowed, None),
            NodeDisposition::Allow
        ));
        assert!(matches!(
            policy.disposition_for_event(&blocked, None),
            NodeDisposition::Deny
        ));
    }
}
