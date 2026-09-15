//! Typed plan-node classification and the shared pure transformation kernel.
//!
//! Classification is exhaustive. External effects cross the runtime's compiled-request
//! transport boundary; plan nodes are never exposed through an adapter trait.

use super::*;
use crate::plasm_plan::{
    ValidatedComputeNode, ValidatedDataNode, ValidatedDeriveNode, ValidatedForEachNode,
    ValidatedIterateUntilNode, ValidatedPlanDataInput, ValidatedPlanNode,
    ValidatedRelationTraversalNode, ValidatedSurfaceNode,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The pure fragment of the plan: transformations whose output rows depend only on already
/// materialized input rows and plan literals. Mode-invariant — evaluated identically in dry and
/// live via [`PureStep::materialize`].
pub(crate) enum PureStep {
    Data(Box<ValidatedDataNode>),
    Derive(Box<ValidatedDeriveNode>),
    Compute(Box<ValidatedComputeNode>),
}

/// Runtime-orchestrated steps: backend reads/effects and their closed control-flow forms.
pub(crate) enum IoStep {
    Surface(Box<ValidatedSurfaceNode>),
    Relation(Box<ValidatedRelationTraversalNode>),
    ForEach(Box<ValidatedForEachNode>),
    IterateUntil(Box<ValidatedIterateUntilNode>),
}

/// Total classification of a validated plan node into the PEC execution taxonomy.
pub(crate) enum ExecStep {
    Pure(PureStep),
    Io(IoStep),
}

/// Resolved inputs a [`PureStep`] needs to produce its rows. The *acquisition* of these rows may be
/// mode-specific (live rehydration vs dry inline stub) — that is the permitted I/O difference — but
/// the transformation over them is not.
pub(crate) struct PureInputs<'a> {
    /// Rows of the step's `source` dependency (empty for [`PureStep::Data`]).
    pub source_rows: &'a [serde_json::Value],
    /// Singleton cross-node inputs (Derive `inputs`; empty otherwise).
    pub input_rows: &'a BTreeMap<InputAlias, MaterializedInputRow>,
    /// Cross-binding row lists for `Plasm.render` compute bindings (empty otherwise).
    pub binding_rows: &'a BTreeMap<String, Vec<serde_json::Value>>,
}

/// Output of the single pure kernel: rows plus the identity/entity metadata the caller needs to
/// wrap them into a [`MaterializedNode`].
pub(crate) struct PureMaterialization {
    pub rows: Vec<serde_json::Value>,
    pub row_identities: Vec<Option<plasm_core::RowIdentity>>,
    pub entity_override: Option<String>,
}

impl ExecStep {
    /// Total lowering: every [`ValidatedPlanNode`] variant maps to exactly one step. The absence of
    /// a wildcard arm is deliberate — it is the compile-time guarantee that no node kind can be
    /// left unclassified (and therefore silently skipped) by either execution mode.
    pub(crate) fn classify(node: ValidatedPlanNode) -> Self {
        match node {
            ValidatedPlanNode::Data(n) => ExecStep::Pure(PureStep::Data(Box::new(n))),
            ValidatedPlanNode::Derive(n) => ExecStep::Pure(PureStep::Derive(Box::new(n))),
            ValidatedPlanNode::Compute(n) => ExecStep::Pure(PureStep::Compute(Box::new(n))),
            ValidatedPlanNode::Surface(n) => ExecStep::Io(IoStep::Surface(Box::new(n))),
            ValidatedPlanNode::RelationTraversal(n) => ExecStep::Io(IoStep::Relation(Box::new(n))),
            ValidatedPlanNode::ForEach(n) => ExecStep::Io(IoStep::ForEach(Box::new(n))),
            ValidatedPlanNode::IterateUntil(n) => ExecStep::Io(IoStep::IterateUntil(Box::new(n))),
        }
    }

    /// Stable tag for the schedule digest (kind + node id), independent of volatile display text.
    pub(crate) fn schedule_tag(&self) -> String {
        match self {
            ExecStep::Pure(p) => format!("pure:{}:{}", p.kind_tag(), p.id().as_str()),
            ExecStep::Io(io) => format!("io:{}:{}", io.kind_tag(), io.id().as_str()),
        }
    }
}

impl PureStep {
    fn kind_tag(&self) -> &'static str {
        match self {
            PureStep::Data(_) => "data",
            PureStep::Derive(_) => "derive",
            PureStep::Compute(_) => "compute",
        }
    }

    pub(crate) fn id(&self) -> &PlanNodeId {
        match self {
            PureStep::Data(n) => &n.id,
            PureStep::Derive(n) => &n.id,
            PureStep::Compute(n) => &n.id,
        }
    }

    /// The dependency whose rows feed this step (`None` for literal `Data`).
    pub(crate) fn source(&self) -> Result<Option<PlanNodeId>, String> {
        match self {
            PureStep::Data(_) => Ok(None),
            PureStep::Derive(d) => Ok(Some(d.source.clone())),
            PureStep::Compute(c) => PlanNodeId::new(c.compute.source.clone()).map(Some),
        }
    }

    /// Singleton cross-node inputs this step broadcasts (Derive only).
    pub(crate) fn inputs(&self) -> &[ValidatedPlanDataInput] {
        match self {
            PureStep::Derive(d) => &d.inputs,
            PureStep::Compute(_) => &[],
            PureStep::Data(_) => &[],
        }
    }

    /// Reconstruct the validated node (for synthetic-node wrapping / fingerprinting on the live
    /// path). Total and lossless — the inverse of the `Pure` arm of [`ExecStep::classify`].
    pub(crate) fn into_validated_node(self) -> ValidatedPlanNode {
        match self {
            PureStep::Data(n) => ValidatedPlanNode::Data(*n),
            PureStep::Derive(n) => ValidatedPlanNode::Derive(*n),
            PureStep::Compute(n) => ValidatedPlanNode::Compute(*n),
        }
    }

    /// Cross-binding row lists for per-row render and plain template Data nodes.
    pub(crate) fn binding_rows(
        &self,
        materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    ) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
        match self {
            PureStep::Compute(c) => binding_rows_for_compute(&c.compute, materialized),
            PureStep::Data(d) => binding_rows_for_data_uses(&d.uses_result, materialized),
            PureStep::Derive(_) => Ok(BTreeMap::new()),
        }
    }

    /// **The** pure kernel. Deterministic, synchronous, and I/O-free: both dry preflight and live
    /// execute produce their pure rows here, so a planned pure node always executes and always
    /// matches its live counterpart (given the same input rows).
    pub(crate) fn materialize(
        &self,
        inputs: &PureInputs<'_>,
        materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    ) -> Result<PureMaterialization, String> {
        match self {
            PureStep::Data(d) => {
                let rows = eval_data_plan_value(&d.data, inputs.binding_rows)?;
                let row_identities = vec![None; rows.len()];
                Ok(PureMaterialization {
                    rows,
                    row_identities,
                    entity_override: None,
                })
            }
            PureStep::Derive(d) => {
                if matches!(d.result_shape, crate::plasm_plan::ResultShape::Single)
                    && inputs.source_rows.len() != 1
                {
                    return Err(super::compute_eval::singleton_input_row_count_error(
                        d.source.as_str(),
                        d.item_binding.as_str(),
                        inputs.source_rows.len(),
                        "scalar field extraction",
                    ));
                }
                let rows = derive_node_rows(
                    &d.item_binding,
                    &d.value,
                    inputs.source_rows,
                    inputs.input_rows,
                )?;
                let row_identities = vec![None; rows.len()];
                Ok(PureMaterialization {
                    rows,
                    row_identities,
                    entity_override: None,
                })
            }
            PureStep::Compute(c) => {
                let rows =
                    eval_compute_from_rows(&c.compute, inputs.source_rows, inputs.binding_rows)?;
                let source = PlanNodeId::new(c.compute.source.clone())?;
                let row_identities =
                    propagate_row_identities(&source, &c.compute.op, materialized, rows.len())?;
                Ok(PureMaterialization {
                    rows,
                    row_identities,
                    entity_override: c.compute.schema.entity.as_deref().map(str::to_string),
                })
            }
        }
    }
}

impl IoStep {
    fn kind_tag(&self) -> &'static str {
        match self {
            IoStep::Surface(_) => "surface",
            IoStep::Relation(_) => "relation",
            IoStep::ForEach(_) => "foreach",
            IoStep::IterateUntil(_) => "iterate_until",
        }
    }

    pub(crate) fn id(&self) -> &PlanNodeId {
        match self {
            IoStep::Surface(n) => &n.id,
            IoStep::Relation(n) => &n.id,
            IoStep::ForEach(n) => &n.id,
            IoStep::IterateUntil(n) => &n.id,
        }
    }
}

/// Content digest of an executable schedule: the ordered `(kind, node id)` of every [`ExecStep`].
/// Derived deterministically from the validated plan, so the dry-run schedule and the `plasm_run`
/// replay schedule are provably identical (the durable commit id already seals `steps` + `bind`,
/// from which this schedule is a total function; this digest is the executable-layer witness of
/// that identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScheduleDigest(pub(crate) [u8; 32]);

impl ScheduleDigest {
    /// Derive the digest from a validated plan by classifying every node in the given topological
    /// execution order. Because [`ExecStep::classify`] is a total function of the node kinds already
    /// sealed by the durable commit id (over `steps` + `bind`), the digest computed at dry-run
    /// (`plasm`) and at replay (`plasm_run`) is identical by construction — it is the
    /// executable-layer witness that both traverse the same schedule.
    pub(crate) fn from_validated_plan(
        plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
        order: &[String],
    ) -> Self {
        let by_id: std::collections::HashMap<&str, &ValidatedPlanNode> =
            plan.nodes.iter().map(|n| (n.id().as_str(), n)).collect();
        let mut hasher = Sha256::new();
        for step_id in order {
            let Some(node) = by_id.get(step_id.as_str()) else {
                continue;
            };
            let step = ExecStep::classify((*node).clone());
            hasher.update(step_id.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(step.schedule_tag().as_bytes());
            hasher.update(b"\n");
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&hasher.finalize());
        ScheduleDigest(bytes)
    }

    pub(crate) fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
