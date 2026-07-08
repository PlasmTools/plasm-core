//! Seams for flow verify: catalog projection and policy evaluation.

use crate::flow_catalog::FlowCatalogView;
use crate::plan_flow::NodeDisposition;
use crate::plan_flow::{QualifiedCapabilityKey, SinkParamRef};
use crate::plan_flow_policy::{
    EffectEvent, FlowPolicy, FlowPolicySnapshot, ForbiddenFlowRule, PolicyRevision,
    SanitizerRecognition,
};
use plasm_core::DataClassName;
use std::collections::BTreeSet;

/// Catalog projection consumed by the flow pass (mockable in tests).
pub trait FlowCatalog {
    fn output_labels(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName>;
    /// Union of output labels across all capabilities for `(entry_id, entity)`.
    fn output_labels_for_entity(&self, entry_id: &str, entity: &str) -> BTreeSet<DataClassName>;
    fn sink_params(&self, key: &QualifiedCapabilityKey) -> &[SinkParamRef];
    fn sanitizers(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName>;
    /// True when any capability in the pinned catalogs produces labeled output.
    fn has_any_output_labels(&self) -> bool;
    /// Behavior-controlling parameter names for robust declassification.
    fn control_params(&self, key: &QualifiedCapabilityKey) -> BTreeSet<String>;
}

impl FlowCatalog for FlowCatalogView {
    fn output_labels(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName> {
        self.output_labels_for(key)
    }

    fn output_labels_for_entity(&self, entry_id: &str, entity: &str) -> BTreeSet<DataClassName> {
        FlowCatalogView::output_labels_for_entity(self, entry_id, entity)
    }

    fn sink_params(&self, key: &QualifiedCapabilityKey) -> &[SinkParamRef] {
        self.sink_params_for(key)
    }

    fn sanitizers(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName> {
        self.sanitizers_for(key)
    }

    fn has_any_output_labels(&self) -> bool {
        !self.capability_output_labels.is_empty()
    }

    fn control_params(&self, key: &QualifiedCapabilityKey) -> BTreeSet<String> {
        self.control_params_for(key)
    }
}

/// Policy evaluation consumed by the flow pass.
pub trait FlowPolicyEvaluator {
    fn policy_revision(&self) -> Option<PolicyRevision>;
    /// IFC forbidden rules — empty when snapshot is Inactive.
    fn forbidden_rules(&self) -> &[ForbiddenFlowRule];
    fn disposition_for_event(
        &self,
        event: &EffectEvent,
        author_label: Option<&str>,
    ) -> NodeDisposition;
    #[allow(dead_code)]
    fn policy_sanitizers(&self) -> &[SanitizerRecognition];
    /// Labels the policy recognizes as cleared by `capability` (union over all matching sanitizer rules).
    fn policy_sanitizer_clears(&self, capability: &str) -> BTreeSet<DataClassName>;
    fn enforces_forbidden(&self) -> bool;
}

/// Adapter holding effective policy for one verify pass (stable forbidden slice).
pub struct FlowPolicyPass<'a> {
    snapshot: &'a FlowPolicySnapshot,
    effective: FlowPolicy,
}

impl<'a> FlowPolicyPass<'a> {
    pub fn new(snapshot: &'a FlowPolicySnapshot) -> Self {
        Self {
            snapshot,
            effective: snapshot.effective_policy(),
        }
    }
}

impl FlowPolicyEvaluator for FlowPolicyPass<'_> {
    fn policy_revision(&self) -> Option<PolicyRevision> {
        match self.snapshot {
            FlowPolicySnapshot::Inactive => None,
            FlowPolicySnapshot::Active { revision, .. } => Some(*revision),
        }
    }

    fn forbidden_rules(&self) -> &[ForbiddenFlowRule] {
        if self.enforces_forbidden() {
            &self.effective.forbidden
        } else {
            &[]
        }
    }

    fn disposition_for_event(
        &self,
        event: &EffectEvent,
        author_label: Option<&str>,
    ) -> NodeDisposition {
        self.effective.disposition_for_event(event, author_label)
    }

    fn policy_sanitizers(&self) -> &[SanitizerRecognition] {
        if matches!(self.snapshot, FlowPolicySnapshot::Active { .. }) {
            &self.effective.sanitizers
        } else {
            &[]
        }
    }

    fn policy_sanitizer_clears(&self, capability: &str) -> BTreeSet<DataClassName> {
        if matches!(self.snapshot, FlowPolicySnapshot::Active { .. }) {
            self.effective.policy_sanitizer_clears(capability)
        } else {
            BTreeSet::new()
        }
    }

    fn enforces_forbidden(&self) -> bool {
        self.snapshot.enforces_forbidden()
    }
}
