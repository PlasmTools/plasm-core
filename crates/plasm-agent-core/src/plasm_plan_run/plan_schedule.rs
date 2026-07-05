//! Bind-graph layer scheduling for parallel comp step execution.

use std::collections::{HashMap, HashSet};

use plasm_core::plasm_monad::{PlasmBindGraph, PlasmStepPayload, StepId};
use plasm_core::EffectClass;

/// Agent-facing bind schedule summary (execution layers + root list from final bind graph).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindExecutionGraphSummary {
    pub execution_layers: Vec<Vec<String>>,
    pub parallelizable_roots: Vec<String>,
}

pub(crate) const PARALLELIZABLE_ROOTS_NOTE: &str =
    "Empty bind.deps only; comp.return.kind parallel is multiple return roots, not concurrent execution.";

/// Derive dry-run bind execution fields from the executable bind graph (single source of truth).
pub(crate) fn bind_execution_graph_summary(
    bind: &PlasmBindGraph,
) -> Result<BindExecutionGraphSummary, String> {
    let execution_layers = bind_topo_execution_layers(bind)?
        .into_iter()
        .map(|layer| layer.iter().map(|s| s.as_str().to_string()).collect())
        .collect();
    let parallelizable_roots = bind
        .topo
        .iter()
        .filter(|id| bind.deps.get(id).map(|d| d.is_empty()).unwrap_or(true))
        .map(|id| id.as_str().to_string())
        .collect();
    Ok(BindExecutionGraphSummary {
        execution_layers,
        parallelizable_roots,
    })
}

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
    if layer.len() <= 1 {
        return false;
    }
    if !layer
        .iter()
        .all(|id| payload_by_step.get(id).is_some_and(comp_step_parallel_safe))
    {
        return false;
    }
    // **CEP-9:** a relation flat-map's source is always an upstream dependency (see
    // `node_dependencies`), so it lands in an earlier layer. A same-layer source means the
    // bind graph dropped that edge; refuse parallel execution in every profile.
    // Linear scan (no allocation); layers are small and relations rare.
    for id in layer {
        let Some(PlasmStepPayload::FlatMapRelation(p)) = payload_by_step.get(id) else {
            continue;
        };
        let source = match StepId::new(p.relation.source.clone()) {
            Ok(source) => source,
            Err(err) => {
                tracing::warn!(
                    target: "plasm_agent::plan_schedule",
                    relation = id.as_str(),
                    source = p.relation.source.as_str(),
                    error = err.as_str(),
                    "CEP-9: relation flat-map has an invalid source id; refusing parallel execution"
                );
                return false;
            }
        };
        if layer.contains(&source) {
            tracing::warn!(
                target: "plasm_agent::plan_schedule",
                relation = id.as_str(),
                source = source.as_str(),
                "CEP-9: relation flat-map shares a layer with its source; refusing parallel execution"
            );
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::plasm_monad::{
        FlatMapRelationPayload, PlanExprIr, PlanQualifiedEntityKey, PlasmStepPayload, PurePayload,
        ResultShape, StepId,
    };
    use plasm_core::RelationMaterialization;
    use std::collections::BTreeSet;

    fn step(id: &str) -> StepId {
        StepId::new(id.to_string()).expect("step id")
    }

    fn pure_payload() -> PlasmStepPayload {
        PlasmStepPayload::Pure(PurePayload {
            data: plasm_core::PlasmDataValue::Literal {
                value: serde_json::Value::Null,
            },
            effect_class: EffectClass::Read,
            result_shape: ResultShape::List,
        })
    }

    fn relation_payload(source: &str) -> PlasmStepPayload {
        PlasmStepPayload::FlatMapRelation(FlatMapRelationPayload {
            relation: plasm_core::PlanRelationTraversal {
                source: source.to_string(),
                relation: "tags".into(),
                target: PlanQualifiedEntityKey {
                    entry_id: "acme".into(),
                    entity: "Tag".into(),
                },
                cardinality: plasm_core::RelationCardinality::Many,
                source_cardinality: plasm_core::RelationSourceCardinality::Single,
                expr: String::new(),
                ir: PlanExprIr {
                    expr: serde_json::json!({"op": "query", "entity": "Tag"}),
                    projection: None,
                    display_expr: None,
                },
                binding_proofs: Vec::new(),
                materialize: Some(RelationMaterialization::FromParentGet { path: vec![] }),
            },
            effect_class: EffectClass::Read,
            result_shape: ResultShape::List,
        })
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
        let a = step("pure_a");
        let b = step("pure_b");
        let bind = PlasmBindGraph {
            topo: vec![a.clone(), b.clone()],
            deps: Default::default(),
            ..Default::default()
        };
        let payload_by_step = HashMap::from([(a, pure_payload()), (b, pure_payload())]);
        let layers = bind_topo_execution_layers(&bind).expect("layers");
        assert_eq!(layers.len(), 1);
        assert!(layer_parallel_safe(&layers[0], &payload_by_step));
    }

    #[test]
    fn cep_9_parallel_layer_rejects_relation_with_source_in_same_layer() {
        let src = step("item");
        let rel = step("tags");
        let bind = PlasmBindGraph {
            topo: vec![src.clone(), rel.clone()],
            deps: Default::default(),
            ..Default::default()
        };
        let payload_by_step = HashMap::from([
            (src.clone(), pure_payload()),
            (rel.clone(), relation_payload(src.as_str())),
        ]);
        let layers = bind_topo_execution_layers(&bind).expect("layers");
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].len(), 2);
        assert!(!layer_parallel_safe(&layers[0], &payload_by_step));
    }

    #[test]
    fn bind_execution_graph_summary_orders_consecutive_writes() {
        let earlier = step("newbranch");
        let later = step("newfile");
        let bind = PlasmBindGraph {
            topo: vec![earlier.clone(), later.clone()],
            deps: [(later.clone(), BTreeSet::from([earlier.clone()]))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let summary = bind_execution_graph_summary(&bind).expect("summary");
        assert_eq!(
            summary.execution_layers,
            vec![vec!["newbranch".to_string()], vec!["newfile".to_string()]]
        );
        assert_eq!(summary.parallelizable_roots, vec!["newbranch".to_string()]);
    }

    #[test]
    fn cep_9_parallel_layer_rejects_relation_with_invalid_source_id() {
        let pure = step("pure");
        let rel = step("tags");
        let payload_by_step = HashMap::from([
            (pure.clone(), pure_payload()),
            (rel.clone(), relation_payload("")),
        ]);
        let layer = vec![pure, rel];

        assert!(
            !layer_parallel_safe(&layer, &payload_by_step),
            "CEP-9: malformed relation source ids must fail closed"
        );
    }
}
