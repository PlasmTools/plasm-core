//! Expand view DAG inner mutators into the PLT flow pass (sink params + existence).

use crate::flow_catalog::FlowCatalogView;
use crate::plan_flow::{
    FlowFacts, FlowViolation, FlowViolationKind, NodeDisposition, NodeFlowFacts,
    QualifiedCapabilityKey, SinkProof,
};
use crate::plan_flow_existence::{apply_unguarded_mutation_review, check_view_existence_flow};
use crate::plan_flow_ports::FlowPolicyEvaluator;
use crate::plan_flow_sanitizer::apply_label_clearance;
use crate::plasm_plan::PlanResultUse;
use plasm_core::schema::{CapabilityKind, ViewDefinition};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct ViewExpandOutcome {
    pub violations: Vec<FlowViolation>,
    pub dispositions: BTreeMap<String, NodeDisposition>,
    pub sink_proofs: BTreeMap<String, SinkProof>,
    pub facts: BTreeMap<String, NodeFlowFacts>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_view_inner_mutations<P: FlowPolicyEvaluator + ?Sized>(
    catalog: &FlowCatalogView,
    policy: &P,
    parent_facts: &NodeFlowFacts,
    entry_id: &str,
    view: &ViewDefinition,
    surface_id: &str,
    uses_result: &[PlanResultUse],
    parent_incoming: FlowFacts,
) -> ViewExpandOutcome {
    let mut out = ViewExpandOutcome {
        violations: Vec::new(),
        dispositions: BTreeMap::new(),
        sink_proofs: BTreeMap::new(),
        facts: BTreeMap::new(),
    };

    let identity_key = catalog
        .capability_workflow_meta(&QualifiedCapabilityKey::from_parts(
            entry_id,
            view.entity.as_str(),
            view.capability.as_str(),
        ))
        .and_then(|m| m.identity_key.clone())
        .unwrap_or_default();

    let existence = check_view_existence_flow(catalog, entry_id, view, &identity_key);
    let synthetic_root = format!("{surface_id}:view");
    let mut disposition = NodeDisposition::Allow;
    apply_unguarded_mutation_review(
        &mut disposition,
        &mut out.violations,
        &synthetic_root,
        existence,
    );
    out.dispositions.insert(synthetic_root.clone(), disposition);

    let mut prior_reads: BTreeSet<String> = BTreeSet::new();
    for node in &view.nodes {
        let inner_key = QualifiedCapabilityKey::from_parts(
            entry_id,
            view.entity.as_str(),
            node.capability.as_str(),
        );
        let Some(meta) = catalog.capability_workflow_meta(&inner_key) else {
            continue;
        };
        if is_read_kind(meta.kind) {
            prior_reads.insert(node.id.clone());
            continue;
        }
        if !is_mutator_kind(meta.kind) {
            continue;
        }

        let synthetic_id = format!("{surface_id}:view:{}", node.id);
        let sink_params = catalog.sink_params_for(&inner_key);
        let mut incoming = parent_incoming.clone();
        if incoming.labels.is_empty() {
            incoming.join(&parent_facts.row_join());
        }

        let mut inner_disposition = NodeDisposition::Allow;
        for forbidden in policy.forbidden_rules() {
            if !incoming.labels.contains(&forbidden.from_label) {
                continue;
            }
            let matches_sink = forbidden.to_sink.as_ref().is_none_or(|sink| {
                sink_params
                    .iter()
                    .any(|param| param.sink_class.as_ref() == Some(sink))
            });
            if matches_sink {
                inner_disposition = NodeDisposition::Deny;
                out.violations.push(FlowViolation {
                    node: synthetic_id.clone(),
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

        let clearance = apply_label_clearance(
            catalog,
            policy,
            &inner_key,
            node.capability.as_str(),
            incoming.labels.clone(),
            None,
            uses_result,
            &out.facts,
            true,
        );
        out.facts.insert(
            synthetic_id.clone(),
            NodeFlowFacts {
                residual: FlowFacts {
                    labels: clearance.outgoing_labels,
                    provenance: incoming.provenance.clone(),
                },
                ..Default::default()
            },
        );
        out.dispositions
            .insert(synthetic_id.clone(), inner_disposition);
        out.sink_proofs.insert(synthetic_id, clearance.proof);
    }

    out
}

fn is_read_kind(kind: CapabilityKind) -> bool {
    matches!(
        kind,
        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
    )
}

fn is_mutator_kind(kind: CapabilityKind) -> bool {
    matches!(
        kind,
        CapabilityKind::Create
            | CapabilityKind::Update
            | CapabilityKind::Delete
            | CapabilityKind::Action
    )
}
