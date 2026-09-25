//! Scoped, read-only correlated body contract for the Python DAG feasibility slice.
//!
//! The body is an ordinary `PlasmComp`, with one declared singleton parent input.
//! This module checks scope/scheduling and cardinality; it does not grant execution
//! authority. The host must still validate CGS ownership, IR and output schemas.
//! Carried by `PlasmStepPayload::MapBody`; capture ports are not executable reads.
use super::{
    comp_semantic_eq, ComputeOp, EffectClass, PlanQualifiedEntityKey, PlasmComp, PlasmReturn,
    PlasmStepPayload, ResultShape, StepId, SurfaceKind, PLASM_COMP_WIRE_VERSION,
};
mod scope;

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::num::NonZeroU32;

/// One captured row, whose entity ownership is retained outside its value fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentCapture {
    /// Outer source rowset. Not a body-local step id.
    pub source: StepId,
    /// Name under which one row is installed in each fresh body scope.
    pub local: StepId,
    pub entity: PlanQualifiedEntityKey,
}

/// One synthetic output row for every parent, including parents with no children.
/// A missing/extra body result is an error, never flat-map/drop semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrelatedBody {
    pub parent: ParentCapture,
    pub max_parents: NonZeroU32,
    pub body: PlasmComp,
}

impl CorrelatedBody {
    /// Check a closed scope and return layers from the shared bind scheduler.
    /// No rows, code or remote reads are evaluated to establish this schedule.
    pub fn execution_layers(&self) -> Result<Vec<Vec<StepId>>, String> {
        if self.parent.source.as_str().trim().is_empty()
            || self.parent.local.as_str().trim().is_empty()
            || self.parent.entity.entry_id.trim().is_empty()
            || self.parent.entity.entity.trim().is_empty()
        {
            return Err(
                "correlated body requires a named, catalog-qualified parent capture".into(),
            );
        }
        if self.body.version != PLASM_COMP_WIRE_VERSION || self.body.steps.is_empty() {
            return Err("correlated body requires a nonempty current-version PlasmComp".into());
        }
        let step_ids: BTreeSet<_> = self.body.steps.keys().map(|s| StepId(s.clone())).collect();
        if step_ids != self.body.bind.topo.iter().cloned().collect() {
            return Err("correlated body steps and bind.topo differ".into());
        }
        let inputs = BTreeSet::from([self.parent.local.clone()]);
        let layers = self.body.bind.execution_layers(&inputs)?;
        for (id, payload) in &self.body.steps {
            if !matches!(
                payload.effect_class(),
                EffectClass::Read | EffectClass::ArtifactRead
            ) {
                return Err(format!("correlated body step {id} must be read-only"));
            }
            // Check the operation as well as its caller-supplied effect label.
            match payload {
                PlasmStepPayload::Invoke(p)
                    if !matches!(
                        p.plan_kind,
                        SurfaceKind::Query | SurfaceKind::Search | SurfaceKind::Get
                    ) =>
                {
                    return Err(format!(
                        "correlated body step {id} has a mutating operation"
                    ));
                }
                PlasmStepPayload::FlatMapApply(_)
                | PlasmStepPayload::UnfoldUntil(_)
                | PlasmStepPayload::MapBody(_) => {
                    return Err(format!("correlated body step {id}: nested effect control flow is outside this slice"));
                }
                _ => {}
            }
            scope::check(&self.body, id, payload)?;
            let mut reads = Vec::new();
            match payload {
                PlasmStepPayload::Map(p) => {
                    reads.push(p.compute.source.as_str());
                    match &p.compute.op {
                        ComputeOp::Union { other } => reads.push(other.as_str()),
                        ComputeOp::Render {
                            render_bindings, ..
                        } => {
                            reads.extend(render_bindings.iter().map(|s| s.as_str()));
                        }
                        _ => {}
                    }
                }
                PlasmStepPayload::Derive(p) => {
                    reads.extend(p.derive.source.as_deref());
                    reads.extend(p.derive.inputs.iter().map(|i| i.node.as_str()));
                }
                PlasmStepPayload::FlatMapRelation(p) => reads.push(p.relation.source.as_str()),
                _ => {}
            }
            let deps = self.body.bind.deps.get(&StepId(id.clone()));
            for read in reads {
                if !deps.is_some_and(|deps| deps.contains(&StepId(read.into()))) {
                    return Err(format!(
                        "correlated body step {id} uses undeclared dependency {read}"
                    ));
                }
            }
        }
        let PlasmReturn::Step { step } = &self.body.return_ else {
            return Err("correlated body must return one synthetic row, not parallel roots".into());
        };
        let output = self
            .body
            .steps
            .get(step.as_str())
            .ok_or("correlated body return is not a local step")?;
        if output.result_shape() != ResultShape::Single {
            return Err("correlated body output must declare a single row".into());
        }
        match output {
            PlasmStepPayload::Pure(_) | PlasmStepPayload::Derive(_) => {}
            PlasmStepPayload::Map(p)
                if p.compute.schema.entity.is_none() && !p.compute.op.preserves_row_identity() => {}
            _ => {
                return Err(
                    "correlated body output must be synthetic, without entity receiver authority"
                        .into(),
                )
            }
        }
        Ok(layers)
    }

    /// Parent admission is fail-before-read; a bound is never an implicit take/limit.
    pub fn check_parent_count(&self, count: usize) -> Result<(), String> {
        if count > self.max_parents.get() as usize {
            return Err(format!(
                "correlated map parent budget exceeded: {count} > {}",
                self.max_parents
            ));
        }
        Ok(())
    }

    /// Called after each body execution, before appending its sole output row.
    pub fn check_output_count(&self, count: usize) -> Result<(), String> {
        if count != 1 {
            return Err(format!(
                "correlated map body must return exactly one row, got {count}"
            ));
        }
        Ok(())
    }

    /// Semantic equality includes scope, qualification and bound; body name/metadata are inert.
    pub fn semantic_eq(&self, other: &Self) -> bool {
        self.parent == other.parent
            && self.max_parents == other.max_parents
            && comp_semantic_eq(&self.body, &other.body)
    }
}

#[cfg(test)]
mod tests;
