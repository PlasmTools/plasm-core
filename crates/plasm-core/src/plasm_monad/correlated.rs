//! Recursive bounded scope contract for Python DAG execution.
//!
//! The body is an ordinary `PlasmComp`, with a singleton parent and typed enclosing inputs.
//! This module checks scope/scheduling and cardinality; it does not grant execution
//! authority. The host must still validate CGS ownership, IR and output schemas.
//! Carried by `PlasmStepPayload::MapBody`; capture ports are not executable reads.
use super::{
    comp_semantic_eq, ComputeOp, EffectClass, EffectEvidence, PlanQualifiedEntityKey, PlasmComp,
    PlasmReturn, PlasmStepPayload, ResultShape, StepId, PLASM_COMP_WIRE_VERSION,
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
    pub output: ScopedOutput,
    pub max_parents: NonZeroU32,
    pub parent_entity_authority: bool,
    pub parent_schema: Option<super::SyntheticResultSchema>,
    pub captures: Vec<ScopedCapture>,
    pub body: PlasmComp,
}

/// Scoped record assembly and rowset bind share the same execution machinery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScopedOutput {
    Record,
    /// A pure Boolean body selects the original parent, preserving its authority.
    Filter,
    /// Short-circuit a pure Boolean body; empty any is false and empty all is true.
    Quantify {
        all: bool,
    },
    Rows {
        entity: PlanQualifiedEntityKey,
        schema: super::SyntheticResultSchema,
        entity_authority: bool,
        acknowledgement: bool,
    },
}

/// An immutable enclosing rowset port, with its admitted value and identity contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedCapture {
    pub source: StepId,
    pub local: StepId,
    pub entity: PlanQualifiedEntityKey,
    pub schema: super::SyntheticResultSchema,
    /// Scalar cells retain their value contract instead of becoming records
    /// merely because the transport also provides a one-column row schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_contract: Option<crate::value_contract::ValueContract>,
    pub singleton: bool,
    pub entity_authority: bool,
}

impl CorrelatedBody {
    /// Check a closed scope and return layers from the shared bind scheduler.
    /// No rows, code or remote reads are evaluated to establish this schedule.
    pub fn execution_layers(&self) -> Result<Vec<Vec<StepId>>, String> {
        self.execution_layers_at_depth(0)
    }

    fn execution_layers_at_depth(&self, depth: usize) -> Result<Vec<Vec<StepId>>, String> {
        if depth >= 16 {
            return Err("scoped composition exceeds 16 map levels".into());
        }
        // All scopes share the execution occurrence budget. Surface operators
        // may impose a smaller authored bound (for example explicit map).
        let maximum = 65_536;
        if self.max_parents.get() > maximum {
            return Err("scope parent bound exceeds execution budget".into());
        }
        if self.parent.source.as_str().trim().is_empty()
            || self.parent.local.as_str().trim().is_empty()
            || self.parent.entity.entry_id.trim().is_empty()
            || self.parent.entity.entity.trim().is_empty()
        {
            return Err(
                "correlated body requires a named, catalog-qualified parent capture".into(),
            );
        }
        if self.body.version != PLASM_COMP_WIRE_VERSION {
            return Err("correlated body requires a current-version PlasmComp".into());
        }
        let step_ids: BTreeSet<_> = self.body.steps.keys().map(|s| StepId(s.clone())).collect();
        if step_ids != self.body.bind.topo.iter().cloned().collect() {
            return Err("correlated body steps and bind.topo differ".into());
        }
        let mut inputs = BTreeSet::from([self.parent.local.clone()]);
        for capture in &self.captures {
            if capture.source.as_str().trim().is_empty()
                || !inputs.insert(capture.local.clone())
                || capture.local.as_str().trim().is_empty()
            {
                return Err("duplicate or empty scoped capture port".into());
            }
        }
        let layers = self.body.bind.execution_layers(&inputs)?;
        if matches!(
            self.output,
            ScopedOutput::Filter | ScopedOutput::Quantify { .. }
        ) && !matches!(
            self.effect_class(),
            EffectClass::Read | EffectClass::ArtifactRead
        ) {
            return Err("predicate scopes cannot contain effects".into());
        }
        for (id, payload) in &self.body.steps {
            match payload {
                PlasmStepPayload::MapBody(child) => {
                    child.execution_layers_at_depth(depth + 1)?;
                }
                PlasmStepPayload::UnfoldUntil(unfold) => {
                    for child in [&unfold.until_scope, &unfold.step_scope]
                        .into_iter()
                        .flatten()
                    {
                        child.execution_layers_at_depth(depth + 1)?;
                    }
                }
                _ => {}
            }
            if let PlasmStepPayload::Invoke(p) = payload {
                let read = matches!(
                    p.plan_kind,
                    super::SurfaceKind::Query
                        | super::SurfaceKind::Search
                        | super::SurfaceKind::Get
                );
                if read
                    && !matches!(
                        p.effect_class,
                        EffectClass::Read | EffectClass::ArtifactRead
                    )
                {
                    return Err("read-only operation has a mutating effect label".into());
                }
                if !read && !matches!(p.effect_class, EffectClass::Write | EffectClass::SideEffect)
                {
                    return Err("mutating operation has a read-only effect label".into());
                }
            }
            scope::check(&self.body, id, payload)?;
            let mut reads = Vec::new();
            match payload {
                PlasmStepPayload::Map(p) => {
                    reads.push(p.compute.source.as_str());
                    match &p.compute.op {
                        ComputeOp::Union { other } | ComputeOp::MergeBranches { other } => {
                            reads.push(other.as_str())
                        }
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
                PlasmStepPayload::FlatMapApply(p) => reads.push(p.source.as_str()),
                PlasmStepPayload::UnfoldUntil(p) => reads.push(p.source.as_str()),
                PlasmStepPayload::MapBody(child) => {
                    reads.push(child.parent.source.as_str());
                    reads.extend(child.captures.iter().map(|c| c.source.as_str()));
                }
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
        // A rowset scope may be the identity on an admitted input port. No
        // synthetic operation is needed; catalog authority and the output schema
        // are still checked against that port by host admission.
        if matches!(self.output, ScopedOutput::Rows { .. }) {
            if !step_ids.contains(step) && !inputs.contains(step) {
                return Err("correlated rowset return is not a local step or capture".into());
            }
            return Ok(layers);
        }
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

    /// Conservative transitive effect classification; payload validation checks labels against IR.
    pub fn effect_class(&self) -> EffectClass {
        let mut result = EffectClass::Read;
        for step in self.body.steps.values() {
            match step.effect_class() {
                EffectClass::SideEffect => return EffectClass::SideEffect,
                EffectClass::Write => result = EffectClass::Write,
                _ => {}
            }
        }
        result
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

    pub fn result_shape(&self) -> ResultShape {
        if matches!(self.output, ScopedOutput::Quantify { .. }) {
            return ResultShape::Single;
        }
        if matches!(
            self.output,
            ScopedOutput::Rows {
                acknowledgement: true,
                ..
            }
        ) {
            ResultShape::SideEffectAck
        } else {
            ResultShape::List
        }
    }

    /// Called after each body execution, before appending its sole output row.
    pub fn check_output_count(&self, count: usize) -> Result<(), String> {
        if !matches!(self.output, ScopedOutput::Rows { .. }) && count != 1 {
            return Err(format!(
                "correlated map body must return exactly one row, got {count}"
            ));
        }
        Ok(())
    }

    /// Semantic equality includes scope, qualification and bound; body name/metadata are inert.
    pub fn semantic_eq(&self, other: &Self) -> bool {
        self.output == other.output
            && self.parent_entity_authority == other.parent_entity_authority
            && self.parent_schema == other.parent_schema
            && self.captures == other.captures
            && self.parent == other.parent
            && self.max_parents == other.max_parents
            && comp_semantic_eq(&self.body, &other.body)
    }
}

#[cfg(test)]
mod tests;

/// A state iteration permits one explicit mutation, preceded by ordinary typed
/// computations/reads. The scope never multiplies or hides its effect budget.
pub fn iteration_step_effect(body: &CorrelatedBody) -> Result<super::EffectTemplate, String> {
    body.execution_layers()?;
    if body.max_parents.get() != 1 || !matches!(body.output, ScopedOutput::Rows { .. }) {
        return Err("iteration step requires a singleton row/effect scope".into());
    }
    let effects = body
        .body
        .steps
        .values()
        .filter(|p| p.is_write_or_side_effect())
        .collect::<Vec<_>>();
    let [PlasmStepPayload::Invoke(operation)] = effects.as_slice() else {
        return Err("iteration step requires exactly one explicit catalog mutation".into());
    };
    if !matches!(
        operation.plan_kind,
        super::SurfaceKind::Create
            | super::SurfaceKind::Update
            | super::SurfaceKind::Delete
            | super::SurfaceKind::Action
    ) {
        return Err("iteration step requires a catalog mutation".into());
    }
    let template = if let Some(template) = &operation.ir_template {
        template.clone()
    } else if let Some(ir) = &operation.ir {
        super::PlanExprTemplate {
            expr: ir.expr.clone(),
            projection: ir.projection.clone(),
            display_expr: ir.display_expr.clone(),
            input_bindings: vec![],
        }
    } else {
        return Err("iteration mutation has no executable IR".into());
    };
    Ok(super::EffectTemplate {
        kind: operation.plan_kind,
        qualified_entity: operation
            .qualified_entity
            .clone()
            .ok_or("iteration mutation has no owner")?,
        expr_template: operation.display_expr.clone().unwrap_or_default(),
        input_bindings: template.input_bindings.clone(),
        ir_template: template,
        effect_class: operation.effect_class,
        result_shape: operation.result_shape,
        projection: operation.projection.clone(),
    })
}
