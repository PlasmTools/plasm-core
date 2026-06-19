//! Bind-graph layer scheduling for parallel comp step execution.

use std::collections::{HashMap, HashSet};

use plasm_core::plasm_monad::{PlasmBindGraph, PlasmStepPayload, StepId};
use plasm_core::EffectClass;

/// Group bind topo steps into layers of mutually ready steps (deps satisfied).
pub(crate) fn bind_topo_execution_layers(
    bind: &PlasmBindGraph,
) -> Result<Vec<Vec<StepId>>, String> {
    let mut remaining: HashSet<StepId> = bind.topo.iter().cloned().collect();
    let mut done: HashSet<StepId> = HashSet::new();
    let mut layers = Vec::new();
    while !remaining.is_empty() {
        let mut layer = Vec::new();
        for id in &bind.topo {
            if !remaining.contains(id) {
                continue;
            }
            let ready = bind
                .deps
                .get(id)
                .map(|deps| deps.iter().all(|d| done.contains(d)))
                .unwrap_or(true);
            if ready {
                layer.push(id.clone());
            }
        }
        if layer.is_empty() {
            return Err(
                "plan bind graph has cyclic or unsatisfiable step dependencies".to_string(),
            );
        }
        for id in &layer {
            remaining.remove(id);
            done.insert(id.clone());
        }
        layers.push(layer);
    }
    Ok(layers)
}

#[must_use]
pub(crate) fn comp_step_parallel_safe(payload: &PlasmStepPayload) -> bool {
    match payload {
        PlasmStepPayload::Invoke(p) => p.effect_class == EffectClass::Read,
        PlasmStepPayload::Pure(_) | PlasmStepPayload::Map(_) | PlasmStepPayload::Derive(_) => true,
        PlasmStepPayload::FlatMapRelation(_) => true,
        PlasmStepPayload::FlatMapEffect(p) => p.effect_class == EffectClass::Read,
    }
}

#[must_use]
pub(crate) fn layer_parallel_safe(
    layer: &[StepId],
    payload_by_step: &HashMap<StepId, PlasmStepPayload>,
) -> bool {
    layer.len() > 1
        && layer
            .iter()
            .all(|id| payload_by_step.get(id).is_some_and(comp_step_parallel_safe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::plasm_monad::StepId;
    use std::collections::BTreeSet;

    fn step(id: &str) -> StepId {
        StepId::new(id.to_string()).expect("step id")
    }

    #[test]
    fn bind_topo_layers_groups_independent_roots() {
        let a = step("a");
        let b = step("b");
        let c = step("c");
        let bind = PlasmBindGraph {
            topo: vec![a.clone(), b.clone(), c.clone()],
            deps: [(c.clone(), BTreeSet::from([a.clone(), b.clone()]))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let layers = bind_topo_execution_layers(&bind).expect("layers");
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].len(), 2);
        assert_eq!(layers[1], vec![c]);
    }

    #[test]
    fn parallel_layer_when_two_pure_steps_share_no_deps() {
        use plasm_core::plasm_monad::{PlasmStepPayload, PurePayload, ResultShape};

        let a = step("pure_a");
        let b = step("pure_b");
        let bind = PlasmBindGraph {
            topo: vec![a.clone(), b.clone()],
            deps: Default::default(),
            ..Default::default()
        };
        let pure = PurePayload {
            data: plasm_core::PlasmDataValue::Literal {
                value: serde_json::Value::Null,
            },
            effect_class: EffectClass::Read,
            result_shape: ResultShape::List,
        };
        let payload_by_step = HashMap::from([
            (a, PlasmStepPayload::Pure(pure.clone())),
            (b, PlasmStepPayload::Pure(pure)),
        ]);
        let layers = bind_topo_execution_layers(&bind).expect("layers");
        assert_eq!(layers.len(), 1);
        assert!(layer_parallel_safe(&layers[0], &payload_by_step));
    }
}
