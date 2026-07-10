//! Typed information-flow pass over validated plans.

use crate::approval_gate::{approval_gate_from_disposition, operation_name_for_kind};
use crate::flow_catalog::FlowCatalogView;
use crate::plan_flow_capability::{
    capability_name_from_expr, resolve_alias_node, resolved_mutation_capability_name,
    surface_capability_key,
};
use crate::plan_flow_policy::{EffectEvent, FlowPolicySnapshot, PolicyRevision};
use crate::plan_flow_ports::{FlowCatalog, FlowPolicyEvaluator, FlowPolicyPass};
use crate::plan_flow_sanitizer::apply_label_clearance;
use crate::plasm_plan::{
    EffectClass, Plan, PlanNodeKind, PlanResultUse, QualifiedEntityKey, ValidatedPlanNode,
    ValidatedPlanState, ValidatedSurfaceNode,
};
use crate::plasm_plan_run::NodeInputHoleIndex;
use plasm_core::{
    CapabilityName, CapabilityParamName, ComputeOp, DataClassName, EntityName, RegistryEntryId,
    SinkClassName,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct QualifiedCapabilityKey {
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
    pub capability: CapabilityName,
}

impl QualifiedCapabilityKey {
    pub fn from_parts(entry_id: &str, entity: &str, capability: &str) -> Self {
        Self {
            entry_id: RegistryEntryId::from(entry_id),
            entity: EntityName::from(entity),
            capability: CapabilityName::from(capability),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SinkParamRef {
    pub param: CapabilityParamName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_class: Option<SinkClassName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct FlowFacts {
    #[serde(default)]
    pub labels: BTreeSet<DataClassName>,
    #[serde(default)]
    pub provenance: BTreeSet<String>,
}

impl FlowFacts {
    pub fn join(&mut self, other: &FlowFacts) {
        self.labels.extend(other.labels.iter().cloned());
        self.provenance.extend(other.provenance.iter().cloned());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct NodeFlowFacts {
    #[serde(default)]
    pub columns: BTreeMap<Vec<String>, FlowFacts>,
    #[serde(default)]
    pub residual: FlowFacts,
}

impl NodeFlowFacts {
    pub fn at_path(&self, path: &[String]) -> FlowFacts {
        if path.is_empty() {
            return self.row_join();
        }
        if let Some(f) = self.columns.get(path) {
            return f.clone();
        }
        self.residual.clone()
    }

    pub fn row_join(&self) -> FlowFacts {
        let mut out = self.residual.clone();
        for f in self.columns.values() {
            out.join(f);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SinkProof {
    StaticClean,
    Deferred {
        check: String,
    },
    Sanitized {
        by: String,
        cleared: BTreeSet<DataClassName>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalRequirement {
    pub policy: crate::plan_flow_policy::ApprovalHostPolicy,
    pub entry_id: String,
    pub entity: String,
    pub capability: String,
    pub policy_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum NodeDisposition {
    Allow,
    Approve { requirement: ApprovalRequirement },
    Review,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FlowVerdict {
    Clean,
    NeedsReview,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowViolationKind {
    ForbiddenFlow,
    UnguardedMutation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowViolation {
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FlowViolationKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_param: Option<SinkParamRef>,
    pub labels: BTreeSet<DataClassName>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanFlowAnalysis {
    pub policy_revision: Option<PolicyRevision>,
    pub verdict: FlowVerdict,
    pub node_facts: BTreeMap<String, NodeFlowFacts>,
    pub node_dispositions: BTreeMap<String, NodeDisposition>,
    pub sink_proofs: BTreeMap<String, SinkProof>,
    pub violations: Vec<FlowViolation>,
}

impl PlanFlowAnalysis {
    pub fn approval_gates_json(&self) -> Vec<serde_json::Value> {
        self.node_dispositions
            .iter()
            .filter_map(|(node, disposition)| approval_gate_from_disposition(node, disposition))
            .collect()
    }

    pub fn approval_gate_for_node(&self, node_id: &str) -> Option<serde_json::Value> {
        self.node_dispositions
            .get(node_id)
            .and_then(|disposition| approval_gate_from_disposition(node_id, disposition))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowCheckedPlan {
    pub analysis: PlanFlowAnalysis,
}

impl FlowCheckedPlan {
    pub fn admit(self) -> Result<FlowAdmission, FlowDenial> {
        match self.analysis.verdict {
            FlowVerdict::Denied => Err(FlowDenial {
                verdict: FlowVerdict::Denied,
                violations: self.analysis.violations,
            }),
            _ => Ok(FlowAdmission::new(self.analysis.policy_revision)),
        }
    }
}

mod flow_admission_seal {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Seal;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowAdmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<PolicyRevision>,
    #[serde(skip)]
    _seal: flow_admission_seal::Seal,
}

impl FlowAdmission {
    fn new(policy_revision: Option<PolicyRevision>) -> Self {
        Self {
            policy_revision,
            _seal: flow_admission_seal::Seal,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowDenial {
    pub verdict: FlowVerdict,
    pub violations: Vec<FlowViolation>,
}

pub fn verify_plan_flow(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
    catalog: &FlowCatalogView,
    snapshot: &FlowPolicySnapshot,
) -> FlowCheckedPlan {
    let policy = FlowPolicyPass::new(snapshot);
    FlowPass {
        plan,
        topological_order,
        catalog,
        policy: &policy,
        topo_index: topological_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect(),
        facts: BTreeMap::new(),
        node_dispositions: BTreeMap::new(),
        sink_proofs: BTreeMap::new(),
        violations: Vec::new(),
    }
    .run()
}

struct MutationFlowCtx<'a> {
    node_id: String,
    qualified: &'a QualifiedEntityKey,
    kind: PlanNodeKind,
    effect_class: EffectClass,
    capability_name: &'a str,
    template_expr: Option<&'a serde_json::Value>,
    expr_template: Option<&'a str>,
    uses_result: &'a [PlanResultUse],
    author_label: Option<&'a str>,
}

struct FlowPass<'a, P: FlowPolicyEvaluator + ?Sized> {
    plan: &'a Plan<ValidatedPlanState>,
    topological_order: &'a [String],
    catalog: &'a FlowCatalogView,
    policy: &'a P,
    topo_index: BTreeMap<String, usize>,
    facts: BTreeMap<String, NodeFlowFacts>,
    node_dispositions: BTreeMap<String, NodeDisposition>,
    sink_proofs: BTreeMap<String, SinkProof>,
    violations: Vec<FlowViolation>,
}

impl<'a, P: FlowPolicyEvaluator + ?Sized> FlowPass<'a, P> {
    fn run(mut self) -> FlowCheckedPlan {
        for node_id in self.topological_order {
            let Some(node) = self.plan.nodes.iter().find(|n| n.id().as_str() == node_id) else {
                continue;
            };
            self.transfer_node(node);
        }
        let verdict = if self
            .node_dispositions
            .values()
            .any(|d| matches!(d, NodeDisposition::Deny))
        {
            FlowVerdict::Denied
        } else if self
            .node_dispositions
            .values()
            .any(|d| matches!(d, NodeDisposition::Review))
        {
            FlowVerdict::NeedsReview
        } else {
            FlowVerdict::Clean
        };
        FlowCheckedPlan {
            analysis: PlanFlowAnalysis {
                policy_revision: self.policy.policy_revision(),
                verdict,
                node_facts: self.facts,
                node_dispositions: self.node_dispositions,
                sink_proofs: self.sink_proofs,
                violations: self.violations,
            },
        }
    }

    fn transfer_node(&mut self, node: &ValidatedPlanNode) {
        let id = node.id().as_str().to_string();
        match node {
            ValidatedPlanNode::Surface(surface) => {
                if is_read_kind(surface.kind) {
                    self.transfer_read_surface(surface);
                } else if is_remote_mutation(surface.kind, surface.effect_class) {
                    self.transfer_mutation_surface(node, surface);
                } else {
                    self.node_dispositions
                        .insert(id.clone(), NodeDisposition::Allow);
                    self.sink_proofs.insert(id, SinkProof::StaticClean);
                }
            }
            ValidatedPlanNode::Compute(n) => {
                let source_id = n.compute.source.as_str().to_string();
                let source_facts = self.facts.get(&source_id).cloned().unwrap_or_default();
                let out = self.transfer_compute(&n.compute.op, &source_facts);
                self.facts.insert(id.clone(), out);
                self.node_dispositions
                    .insert(id.clone(), NodeDisposition::Allow);
                self.sink_proofs.insert(id, SinkProof::StaticClean);
            }
            ValidatedPlanNode::Derive(n) => {
                let source_facts = self
                    .facts
                    .get(n.source.as_str())
                    .cloned()
                    .unwrap_or_default();
                self.facts.insert(id.clone(), source_facts);
                self.node_dispositions
                    .insert(id.clone(), NodeDisposition::Allow);
                self.sink_proofs.insert(id, SinkProof::StaticClean);
            }
            ValidatedPlanNode::Data(_) => {
                self.facts.insert(id.clone(), NodeFlowFacts::default());
                self.node_dispositions
                    .insert(id.clone(), NodeDisposition::Allow);
                self.sink_proofs.insert(id, SinkProof::StaticClean);
            }
            ValidatedPlanNode::ForEach(n) => {
                let source_facts = self
                    .facts
                    .get(n.source.as_str())
                    .cloned()
                    .unwrap_or_default();
                self.facts.insert(id.clone(), source_facts);
                let cap_name = capability_name_from_expr(&n.effect_template.ir_template.expr)
                    .unwrap_or_else(|| operation_name_for_kind(n.effect_template.kind).to_string());
                self.transfer_mutation_template(MutationFlowCtx {
                    node_id: id.clone(),
                    qualified: &n.effect_template.qualified_entity,
                    kind: n.effect_template.kind,
                    effect_class: n.effect_template.effect_class,
                    capability_name: cap_name.as_str(),
                    template_expr: Some(&n.effect_template.ir_template.expr),
                    expr_template: Some(n.effect_template.expr_template.as_str()),
                    uses_result: &n.uses_result,
                    author_label: n.approval.as_deref(),
                });
            }
            ValidatedPlanNode::RelationTraversal(n) => {
                let parent_facts = self
                    .facts
                    .get(n.relation.source.as_str())
                    .cloned()
                    .unwrap_or_default();
                self.facts.insert(id.clone(), parent_facts);
                self.node_dispositions
                    .insert(id.clone(), NodeDisposition::Allow);
                self.sink_proofs.insert(id, SinkProof::StaticClean);
            }
        }
    }

    fn transfer_read_surface(&mut self, surface: &ValidatedSurfaceNode) {
        let id = surface.id.as_str().to_string();
        let Some(key) = surface_capability_key(surface) else {
            self.facts.insert(id.clone(), NodeFlowFacts::default());
            self.node_dispositions
                .insert(id.clone(), NodeDisposition::Allow);
            self.sink_proofs.insert(id, SinkProof::StaticClean);
            return;
        };
        let mut labels = self.catalog.output_labels(&key);
        if labels.is_empty() {
            labels = self
                .catalog
                .output_labels_for_entity(key.entry_id.as_str(), key.entity.as_str());
        }
        let cap_name = key.capability.as_str();
        let clearance = apply_label_clearance(
            self.catalog,
            self.policy,
            &key,
            cap_name,
            labels,
            None,
            &[],
            &self.facts,
            false,
        );
        let provenance_key = format!(
            "{}.{}.{}",
            key.entry_id.as_str(),
            key.entity.as_str(),
            key.capability.as_str()
        );
        let facts = NodeFlowFacts {
            residual: FlowFacts {
                labels: clearance.outgoing_labels,
                provenance: std::iter::once(provenance_key).collect(),
            },
            ..Default::default()
        };
        self.facts.insert(id.clone(), facts);
        self.node_dispositions
            .insert(id.clone(), NodeDisposition::Allow);
        self.sink_proofs.insert(id.clone(), clearance.proof);

        if let Some(view_key) = self.catalog.capability_view_key(&key) {
            if let Some(view) = self
                .catalog
                .view_definition(key.entry_id.as_str(), view_key)
            {
                let parent_incoming = FlowFacts {
                    labels: self
                        .facts
                        .get(id.as_str())
                        .map(|f| f.row_join().labels)
                        .unwrap_or_default(),
                    provenance: BTreeSet::new(),
                };
                let expanded = crate::plan_flow_view_expand::expand_view_inner_mutations(
                    self.catalog,
                    self.policy,
                    self.facts.get(id.as_str()).unwrap_or(&NodeFlowFacts::default()),
                    key.entry_id.as_str(),
                    view,
                    id.as_str(),
                    &surface.uses_result,
                    parent_incoming,
                );
                self.violations.extend(expanded.violations);
                self.node_dispositions.extend(expanded.dispositions);
                self.sink_proofs.extend(expanded.sink_proofs);
                self.facts.extend(expanded.facts);
            }
        }
    }

    fn transfer_mutation_surface(
        &mut self,
        node: &ValidatedPlanNode,
        surface: &ValidatedSurfaceNode,
    ) {
        let id = surface.id.as_str().to_string();
        let template_expr = surface.ir_template.as_ref().map(|t| &t.expr);
        let Some(q) = surface.qualified_entity.as_ref() else {
            self.node_dispositions
                .insert(id.clone(), policy_disposition_for_node(self.policy, node));
            self.sink_proofs.insert(id, SinkProof::StaticClean);
            return;
        };
        let cap_name = resolved_mutation_capability_name(template_expr, surface.kind);
        self.transfer_mutation_template(MutationFlowCtx {
            node_id: id,
            qualified: q,
            kind: surface.kind,
            effect_class: surface.effect_class,
            capability_name: cap_name.as_str(),
            template_expr,
            expr_template: surface.display_expr.as_deref(),
            uses_result: &surface.uses_result,
            author_label: surface.approval.as_deref(),
        });
    }

    fn transfer_mutation_template(&mut self, ctx: MutationFlowCtx<'_>) {
        let key = QualifiedCapabilityKey::from_parts(
            ctx.qualified.entry_id.as_str(),
            ctx.qualified.entity.as_str(),
            ctx.capability_name,
        );
        let sink_params = self.catalog.sink_params(&key);
        let mut incoming = ctx
            .template_expr
            .map(|expr| self.incoming_facts_from_template(expr, ctx.uses_result))
            .unwrap_or_default();
        if incoming.labels.is_empty() {
            for u in ctx.uses_result {
                if let Some(facts) = self.facts.get(u.node.as_str()) {
                    incoming.join(&facts.row_join());
                }
            }
        }
        let event = EffectEvent::from_mutation(
            ctx.qualified,
            ctx.kind,
            ctx.effect_class,
            ctx.capability_name,
            ctx.expr_template,
        );
        let mut disposition = self.policy.disposition_for_event(&event, ctx.author_label);
        for forbidden in self.policy.forbidden_rules() {
            if !incoming.labels.contains(&forbidden.from_label) {
                continue;
            }
            let matches_sink = forbidden.to_sink.as_ref().is_none_or(|sink| {
                sink_params
                    .iter()
                    .any(|param| param.sink_class.as_ref() == Some(sink))
            });
            if matches_sink {
                disposition = NodeDisposition::Deny;
                self.violations.push(FlowViolation {
                    node: ctx.node_id.clone(),
                    kind: Some(FlowViolationKind::ForbiddenFlow),
                    sink_param: sink_params.first().cloned(),
                    labels: incoming.labels.clone(),
                    reason: forbidden
                        .reason
                        .clone()
                        .unwrap_or_else(|| "forbidden label-to-sink flow".to_string()),
                });
            }
        }
        let existence = crate::plan_flow_existence::check_plan_mutation_existence(
            self.catalog,
            self.plan,
            &self.topo_index,
            &ctx.node_id,
            ctx.qualified.entry_id.as_str(),
            ctx.qualified.entity.as_str(),
            ctx.capability_name,
            ctx.template_expr,
            ctx.uses_result,
        );
        let mut disposition = disposition;
        crate::plan_flow_existence::apply_unguarded_mutation_review(
            &mut disposition,
            &mut self.violations,
            &ctx.node_id,
            existence,
        );

        self.node_dispositions
            .insert(ctx.node_id.clone(), disposition.clone());

        let clearance = apply_label_clearance(
            self.catalog,
            self.policy,
            &key,
            ctx.capability_name,
            incoming.labels.clone(),
            ctx.template_expr,
            ctx.uses_result,
            &self.facts,
            true,
        );
        self.facts.insert(
            ctx.node_id.clone(),
            NodeFlowFacts {
                residual: FlowFacts {
                    labels: clearance.outgoing_labels,
                    provenance: incoming.provenance.clone(),
                },
                ..Default::default()
            },
        );
        self.sink_proofs.insert(ctx.node_id, clearance.proof);
    }

    fn incoming_facts_from_template(
        &self,
        expr: &serde_json::Value,
        uses_result: &[PlanResultUse],
    ) -> FlowFacts {
        let holes = NodeInputHoleIndex::from_template_expr(expr);
        let mut incoming = FlowFacts::default();
        for (alias, paths) in holes.alias_paths() {
            let Some(source_id) = resolve_alias_node(uses_result, alias) else {
                continue;
            };
            let source_facts = self
                .facts
                .get(source_id.as_str())
                .cloned()
                .unwrap_or_default();
            for path in paths {
                incoming.join(&source_facts.at_path(path));
            }
        }
        incoming
    }

    fn transfer_compute(&self, op: &ComputeOp, source_facts: &NodeFlowFacts) -> NodeFlowFacts {
        let mut out = NodeFlowFacts::default();
        match op {
            ComputeOp::Project { fields, .. } => {
                for (out_name, src_path) in fields {
                    let path: Vec<String> = src_path
                        .segments()
                        .iter()
                        .map(|s| s.as_str().to_string())
                        .collect();
                    out.columns.insert(
                        vec![out_name.as_str().to_string()],
                        source_facts.at_path(&path),
                    );
                }
            }
            ComputeOp::Filter { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::Limit { .. }
            | ComputeOp::DedupeBy { .. } => {
                out = source_facts.clone();
            }
            ComputeOp::GroupBy { aggregates, .. } | ComputeOp::Aggregate { aggregates, .. } => {
                for agg in aggregates {
                    let facts = agg
                        .field
                        .as_ref()
                        .map(|f| {
                            let path: Vec<String> = f
                                .segments()
                                .iter()
                                .map(|s| s.as_str().to_string())
                                .collect();
                            source_facts.at_path(&path)
                        })
                        .unwrap_or_else(|| source_facts.row_join());
                    out.columns
                        .insert(vec![agg.name.as_str().to_string()], facts);
                }
            }
            ComputeOp::Render { .. } => {
                out.residual = source_facts.row_join();
            }
        }
        out
    }
}

fn policy_disposition_for_node<P: FlowPolicyEvaluator + ?Sized>(
    policy: &P,
    node: &ValidatedPlanNode,
) -> NodeDisposition {
    match node {
        ValidatedPlanNode::Surface(n) if is_remote_mutation(n.kind, n.effect_class) => {
            let Some(q) = n.qualified_entity.as_ref() else {
                return NodeDisposition::Allow;
            };
            let cap_name = n
                .ir_template
                .as_ref()
                .map(|t| resolved_mutation_capability_name(Some(&t.expr), n.kind))
                .unwrap_or_else(|| operation_name_for_kind(n.kind).to_string());
            let event = EffectEvent::from_mutation(
                q,
                n.kind,
                n.effect_class,
                cap_name.as_str(),
                n.display_expr.as_deref(),
            );
            policy.disposition_for_event(&event, n.approval.as_deref())
        }
        ValidatedPlanNode::ForEach(n)
            if is_remote_mutation(n.effect_template.kind, n.effect_template.effect_class) =>
        {
            let cap_name = capability_name_from_expr(&n.effect_template.ir_template.expr)
                .unwrap_or_else(|| operation_name_for_kind(n.effect_template.kind).to_string());
            let event = EffectEvent::from_mutation(
                &n.effect_template.qualified_entity,
                n.effect_template.kind,
                n.effect_template.effect_class,
                cap_name.as_str(),
                Some(n.effect_template.expr_template.as_str()),
            );
            policy.disposition_for_event(&event, n.approval.as_deref())
        }
        _ => NodeDisposition::Allow,
    }
}

fn is_read_kind(kind: PlanNodeKind) -> bool {
    matches!(
        kind,
        PlanNodeKind::Query | PlanNodeKind::Search | PlanNodeKind::Get
    )
}

fn is_remote_mutation(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    matches!(
        kind,
        PlanNodeKind::Create | PlanNodeKind::Update | PlanNodeKind::Delete | PlanNodeKind::Action
    ) || matches!(effect_class, EffectClass::Write | EffectClass::SideEffect)
}

#[cfg(test)]
#[path = "plan_flow_tests.rs"]
mod tests;
