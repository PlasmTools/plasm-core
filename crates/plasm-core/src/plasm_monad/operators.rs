use super::bind_graph::PlasmBindGraph;
use super::comp::{PlasmComp, PlasmReturn, StepId};
use super::payload::{
    ComputeOp, ComputeTemplate, InvokePayload, MapPayload, PlasmStepPayload, SyntheticResultSchema,
};
use super::step::{EffectClass, ResultShape, SurfaceKind};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompBuildError {
    DuplicateStep(StepId),
    UnknownSource(StepId),
}

impl std::fmt::Display for CompBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateStep(id) => write!(f, "duplicate step {id}"),
            Self::UnknownSource(id) => write!(f, "unknown bind source {id}"),
        }
    }
}

pub fn plasm_pure_step(
    comp: &mut PlasmComp,
    id: StepId,
    step: PlasmStepPayload,
    _summary: &str,
) -> Result<(), CompBuildError> {
    if comp.steps.contains_key(id.as_str()) {
        return Err(CompBuildError::DuplicateStep(id));
    }
    comp.steps.insert(id.as_str().to_string(), step);
    comp.bind.topo.push(id.clone());
    Ok(())
}

pub fn plasm_bind_step(
    comp: &mut PlasmComp,
    id: StepId,
    primary: StepId,
    step: PlasmStepPayload,
    extra_deps: &[StepId],
) -> Result<(), CompBuildError> {
    if !comp.steps.contains_key(primary.as_str()) && primary.as_str() != id.as_str() {
        return Err(CompBuildError::UnknownSource(primary));
    }
    if comp.steps.contains_key(id.as_str()) {
        return Err(CompBuildError::DuplicateStep(id));
    }
    comp.steps.insert(id.as_str().to_string(), step);
    comp.bind.primary.insert(id.clone(), primary.clone());
    let deps = comp.bind.deps.entry(id.clone()).or_default();
    deps.insert(primary);
    for d in extra_deps {
        deps.insert(d.clone());
    }
    comp.bind.topo.push(id);
    Ok(())
}

pub fn plasm_map_step(
    comp: &mut PlasmComp,
    id: StepId,
    source: StepId,
    _op: &str,
    step: PlasmStepPayload,
) -> Result<(), CompBuildError> {
    plasm_bind_step(comp, id, source, step, &[])
}

pub fn plasm_parallel_return(steps: Vec<StepId>) -> Result<PlasmReturn, String> {
    if steps.is_empty() {
        return Err("parallel return requires at least one step".into());
    }
    if steps.len() == 1 {
        return Ok(PlasmReturn::Step {
            step: steps.into_iter().next().expect("one"),
        });
    }
    Ok(PlasmReturn::Parallel { steps })
}

/// Construct an empty comp shell for incremental bind construction.
pub fn empty_comp(name: Option<String>) -> PlasmComp {
    PlasmComp {
        version: 1,
        name,
        steps: BTreeMap::new(),
        bind: PlasmBindGraph::default(),
        return_: PlasmReturn::Step {
            step: StepId::new("_unset").expect("sentinel"),
        },
        metadata: BTreeMap::new(),
    }
}

/// Helper for tests: invoke step payload.
pub fn invoke_step_payload(
    plan_kind: SurfaceKind,
    effect: EffectClass,
    shape: ResultShape,
    operation: &str,
) -> PlasmStepPayload {
    PlasmStepPayload::Invoke(InvokePayload {
        plan_kind,
        qualified_entity: None,
        ir: None,
        ir_template: None,
        projection: vec![],
        predicates: vec![],
        page_size: None,
        approval: None,
        display_expr: Some(operation.to_string()),
        effect_class: effect,
        result_shape: shape,
    })
}

/// Helper for tests: map/compute step payload.
pub fn map_step_payload(source: &str, op: ComputeOp, shape: ResultShape) -> PlasmStepPayload {
    PlasmStepPayload::Map(MapPayload {
        compute: ComputeTemplate {
            source: source.to_string(),
            op,
            schema: SyntheticResultSchema {
                entity: None,
                fields: vec![],
            },
            page_size: None,
            collection_alias: None,
        },
        effect_class: EffectClass::Read,
        result_shape: shape,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_chain_builds_topo() {
        let mut comp = empty_comp(Some("test".into()));
        plasm_pure_step(
            &mut comp,
            StepId::new("a").unwrap(),
            invoke_step_payload(
                SurfaceKind::Query,
                EffectClass::Read,
                ResultShape::List,
                "query Item",
            ),
            "a",
        )
        .unwrap();
        plasm_map_step(
            &mut comp,
            StepId::new("b").unwrap(),
            StepId::new("a").unwrap(),
            "limit 2",
            map_step_payload("a", ComputeOp::Limit { count: 2 }, ResultShape::List),
        )
        .unwrap();
        comp.return_ = PlasmReturn::Step {
            step: StepId::new("b").unwrap(),
        };
        assert_eq!(comp.bind.topo.len(), 2);
        assert!(comp.validate().is_ok());
    }
}
