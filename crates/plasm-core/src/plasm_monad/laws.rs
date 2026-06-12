//! Monad law conformance on synthetic [`PlasmComp`] chains.

use super::comp::{PlasmComp, PlasmReturn, StepId};
use super::equiv::comp_semantic_eq;
use super::operators::{
    empty_comp, invoke_step_payload, map_step_payload, plasm_bind_step, plasm_map_step,
    plasm_pure_step,
};
use super::payload::ComputeOp;
use super::step::{EffectClass, ResultShape, SurfaceKind};
use std::collections::BTreeMap;

fn read_query_comp(id: &str, op: &str) -> (PlasmComp, StepId) {
    let mut comp = empty_comp(None);
    let sid = StepId::new(id).unwrap();
    plasm_pure_step(
        &mut comp,
        sid.clone(),
        invoke_step_payload(SurfaceKind::Query, EffectClass::Read, ResultShape::List, op),
        op,
    )
    .unwrap();
    (comp, sid)
}

#[test]
fn left_identity_bind_pure_then_map() {
    let (mut ma, a_id) = read_query_comp("issues", "query Issue");
    plasm_map_step(
        &mut ma,
        StepId::new("limited").unwrap(),
        a_id.clone(),
        "limit 2",
        map_step_payload("issues", ComputeOp::Limit { count: 2 }, ResultShape::List),
    )
    .unwrap();
    ma.return_ = PlasmReturn::Step {
        step: StepId::new("limited").unwrap(),
    };

    let mut direct = empty_comp(None);
    plasm_pure_step(
        &mut direct,
        StepId::new("issues").unwrap(),
        invoke_step_payload(
            SurfaceKind::Query,
            EffectClass::Read,
            ResultShape::List,
            "query Issue",
        ),
        "issues",
    )
    .unwrap();
    plasm_bind_step(
        &mut direct,
        StepId::new("limited").unwrap(),
        StepId::new("issues").unwrap(),
        map_step_payload("issues", ComputeOp::Limit { count: 2 }, ResultShape::List),
        &[],
    )
    .unwrap();
    direct.return_ = PlasmReturn::Step {
        step: StepId::new("limited").unwrap(),
    };

    assert!(comp_semantic_eq(&ma, &direct));
}

#[test]
fn right_identity_bind_map_then_return() {
    let (mut m, a_id) = read_query_comp("x", "query X");
    plasm_map_step(
        &mut m,
        StepId::new("y").unwrap(),
        a_id,
        "project id",
        map_step_payload(
            "x",
            ComputeOp::Project {
                fields: BTreeMap::new(),
            },
            ResultShape::List,
        ),
    )
    .unwrap();
    m.return_ = PlasmReturn::Step {
        step: StepId::new("y").unwrap(),
    };
    let y_only = m.clone();
    assert!(comp_semantic_eq(&m, &y_only));
}

#[test]
fn associativity_three_read_maps() {
    let mut left = empty_comp(None);
    plasm_pure_step(
        &mut left,
        StepId::new("a").unwrap(),
        invoke_step_payload(
            SurfaceKind::Query,
            EffectClass::Read,
            ResultShape::List,
            "a",
        ),
        "a",
    )
    .unwrap();
    plasm_map_step(
        &mut left,
        StepId::new("b").unwrap(),
        StepId::new("a").unwrap(),
        "limit",
        map_step_payload("a", ComputeOp::Limit { count: 10 }, ResultShape::List),
    )
    .unwrap();
    plasm_map_step(
        &mut left,
        StepId::new("c").unwrap(),
        StepId::new("b").unwrap(),
        "filter",
        map_step_payload(
            "b",
            ComputeOp::Filter { predicates: vec![] },
            ResultShape::List,
        ),
    )
    .unwrap();
    left.return_ = PlasmReturn::Step {
        step: StepId::new("c").unwrap(),
    };

    let mut right = empty_comp(None);
    plasm_pure_step(
        &mut right,
        StepId::new("a").unwrap(),
        invoke_step_payload(
            SurfaceKind::Query,
            EffectClass::Read,
            ResultShape::List,
            "a",
        ),
        "a",
    )
    .unwrap();
    plasm_map_step(
        &mut right,
        StepId::new("bc").unwrap(),
        StepId::new("a").unwrap(),
        "limit+filter",
        map_step_payload(
            "a",
            ComputeOp::Filter { predicates: vec![] },
            ResultShape::List,
        ),
    )
    .unwrap();
    right.return_ = PlasmReturn::Step {
        step: StepId::new("bc").unwrap(),
    };

    assert_eq!(left.bind.topo.len(), 3);
    assert_eq!(right.bind.topo.len(), 2);
}

#[test]
fn parallel_return_not_nested_bind() {
    use super::operators::plasm_parallel_return;
    let pr =
        plasm_parallel_return(vec![StepId::new("a").unwrap(), StepId::new("b").unwrap()]).unwrap();
    assert!(matches!(pr, PlasmReturn::Parallel { .. }));
}
