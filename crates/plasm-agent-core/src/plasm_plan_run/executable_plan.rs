//! Plan Executability Closure (PEC): the typed executable schedule DERIVED from a validated plan.
//!
//! # Invariant
//!
//! Every planned Plasm program must execute; the **only** admissible difference between the dry
//! preflight and live execute is I/O. This module makes that invariant structural rather than
//! aspirational: a [`ValidatedPlanNode`] is lowered — *totally, by construction* — into exactly one
//! [`ExecStep`]:
//!
//! * [`ExecStep::Pure`] — a [`PureStep`] whose rows are a deterministic function of already
//!   materialized inputs. There is a **single** kernel ([`PureStep::materialize`]) and both the dry
//!   walk and the live per-step materializer call it. No second pure interpreter exists, so a
//!   planned pure node cannot be silently dropped or diverge from its live counterpart.
//! * [`ExecStep::Io`] — an [`IoStep`] executed through the [`IoPort`] trait. This is the *only*
//!   place dry and live differ: [`DryIoPort`](super::compute_eval::DryIoPort) yields typed stub rows,
//!   [`LiveIoPort`](super::step_materialize::LiveIoPort) performs the real network effect.
//!
//! [`ExecStep::classify`] is a total match with **no wildcard** — adding a `ValidatedPlanNode`
//! variant is a compile error until it is classified as pure or I/O. Execution is therefore a typed
//! artefact derived from a validated plan: no free functions, no guesses.

use super::*;
use crate::plasm_plan::{
    ValidatedComputeNode, ValidatedDataNode, ValidatedDeriveNode, ValidatedForEachNode,
    ValidatedPlanDataInput, ValidatedPlanNode, ValidatedRelationTraversalNode,
    ValidatedSurfaceNode,
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

/// The I/O fragment of the plan: steps that reach a backend (read or effect). Executed through the
/// [`IoPort`] seam so that dry (stub) and live (real) diverge in exactly one typed place.
pub(crate) enum IoStep {
    Surface(Box<ValidatedSurfaceNode>),
    Relation(Box<ValidatedRelationTraversalNode>),
    ForEach(Box<ValidatedForEachNode>),
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
            PureStep::Data(_) | PureStep::Compute(_) => &[],
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

    /// Cross-binding row lists for a `Plasm.render` compute (empty for every other step).
    pub(crate) fn binding_rows(
        &self,
        materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    ) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
        match self {
            PureStep::Compute(c) => binding_rows_for_render(&c.compute, materialized),
            PureStep::Data(_) | PureStep::Derive(_) => Ok(BTreeMap::new()),
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
                let rows = plan_value_to_rows(&d.data)?;
                let row_identities = vec![None; rows.len()];
                Ok(PureMaterialization {
                    rows,
                    row_identities,
                    entity_override: None,
                })
            }
            PureStep::Derive(d) => {
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
        }
    }

    pub(crate) fn id(&self) -> &PlanNodeId {
        match self {
            IoStep::Surface(n) => &n.id,
            IoStep::Relation(n) => &n.id,
            IoStep::ForEach(n) => &n.id,
        }
    }
}

/// The single typed seam across which dry and live execution diverge. Implemented by
/// [`LiveIoPort`](super::step_materialize::LiveIoPort) (real backend effects) and
/// [`DryIoPort`](super::compute_eval::DryIoPort) (typed stub rows).
#[async_trait::async_trait]
pub(crate) trait IoPort {
    /// Produce the materialized node for an I/O step. Returns `None` only when the step has nothing
    /// to materialize in this mode — e.g. a dry stub over an entity-optional surface or a
    /// foreign-catalog effect target that is not loaded in the session. Live execute always
    /// materializes (`Some`); it fails loudly rather than skipping.
    async fn materialize_io(
        &self,
        step: &IoStep,
        step_idx: usize,
        materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    ) -> Result<Option<MaterializedNode>, String>;
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
