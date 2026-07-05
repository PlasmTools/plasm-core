//! Build [`PlasmComp`] wire artifacts from validated plan nodes (single compile path).

use crate::plasm_plan::{
    EffectClass, PlanResultUse, ValidatedPlan, ValidatedPlanNode, ValidatedPlanReturn,
};
use crate::plasm_plan_run::{node_dependencies, DryPlasmPlanEvaluation};
use crate::plasm_step_convert::validated_node_to_step_payload;
pub use plasm_core::plasm_monad::PlasmCompArtifact;
use plasm_core::plasm_monad::{PlasmBindGraph, PlasmComp, PlasmHoleUse, PlasmReturn, StepId};
use plasm_trace::TraceCompWire;
use std::collections::{BTreeMap, BTreeSet};

pub fn plasm_comp_artifact_from_comp(comp: PlasmComp) -> Result<PlasmCompArtifact, String> {
    comp.validate()?;
    let mut approval_gates = Vec::new();
    for (id, step) in &comp.steps {
        let needs_gate = match step {
            plasm_core::PlasmStepPayload::Invoke(p) => p.approval.is_some(),
            plasm_core::PlasmStepPayload::FlatMapEffect(p) => p.approval.is_some(),
            _ => false,
        };
        if needs_gate {
            approval_gates.push(StepId::new(id.clone())?);
        }
    }
    Ok(PlasmCompArtifact {
        comp,
        approval_gates,
    })
}

pub fn plasm_comp_from_validated(validated: &ValidatedPlan) -> PlasmCompArtifact {
    let plan = validated.artifact();
    let mut steps = BTreeMap::new();
    for node in validated.nodes() {
        let id = node.id().as_str().to_string();
        let payload = validated_node_to_step_payload(node)
            .unwrap_or_else(|e| panic!("plasm_comp_from_validated: step {id}: {e}"));
        steps.insert(id, payload);
    }
    let bind = build_bind_graph(validated);
    let return_ = plasm_return_from_validated(&plan.return_value);
    let mut metadata = plan.metadata.clone();
    metadata.insert("language".into(), serde_json::json!("plasm-comp"));
    if !bind.program_order_write_deps.is_empty() {
        metadata.insert(
            "program_order_write_deps".into(),
            serde_json::json!(&bind.program_order_write_deps),
        );
    }
    let comp = PlasmComp {
        version: plan.version,
        name: plan.name.clone(),
        steps,
        bind: bind.graph,
        return_,
        metadata,
    };
    PlasmCompArtifact {
        comp,
        approval_gates: validated
            .approval_gates()
            .iter()
            .map(|id| StepId::new(id.as_str().to_string()).expect("approval gate id"))
            .collect(),
    }
}

fn plasm_return_from_validated(ret: &ValidatedPlanReturn) -> PlasmReturn {
    match ret {
        ValidatedPlanReturn::Node(id) => PlasmReturn::Step {
            step: StepId::new(id.as_str().to_string()).expect("return step id"),
        },
        ValidatedPlanReturn::Parallel { parallel } => PlasmReturn::Parallel {
            steps: parallel
                .iter()
                .map(|id| StepId::new(id.as_str().to_string()).expect("parallel step"))
                .collect(),
        },
    }
}

fn build_bind_graph(validated: &ValidatedPlan) -> BuiltBindGraph {
    let topo: Vec<StepId> = validated
        .topological_order()
        .iter()
        .map(|id| StepId::new(id.as_str().to_string()).expect("step id"))
        .collect();
    let mut deps = BTreeMap::new();
    let mut primary = BTreeMap::new();
    let mut holes = BTreeMap::new();
    for node in validated.nodes() {
        let id = StepId::new(node.id().as_str().to_string()).expect("step id");
        let dep_set: BTreeSet<StepId> = node_dependencies(node)
            .into_iter()
            .filter_map(|d| StepId::new(d).ok())
            .collect();
        if !dep_set.is_empty() {
            deps.insert(id.clone(), dep_set.clone());
        }
        if let Some(p) = primary_predecessor(node) {
            if let Ok(ps) = StepId::new(p) {
                primary.insert(id.clone(), ps);
            }
        }
        let hole_uses: Vec<PlasmHoleUse> = node
            .uses_result()
            .iter()
            .filter_map(|u: &PlanResultUse| {
                Some(PlasmHoleUse {
                    step: StepId::new(u.node.clone()).ok()?,
                    alias: u.r#as.clone(),
                })
            })
            .collect();
        if !hole_uses.is_empty() {
            holes.insert(id, hole_uses);
        }
    }
    let program_order_write_deps = add_consecutive_write_program_order_deps(validated, &mut deps);
    BuiltBindGraph {
        graph: PlasmBindGraph {
            topo,
            deps,
            primary,
            holes,
        },
        program_order_write_deps,
    }
}

struct BuiltBindGraph {
    graph: PlasmBindGraph,
    /// Pairs `[earlier_write, later_write]` added for program-order scheduling (not dataflow).
    program_order_write_deps: Vec<[String; 2]>,
}

/// Synthetic bind edges between consecutive write/side-effect steps in program order.
fn add_consecutive_write_program_order_deps(
    validated: &ValidatedPlan,
    deps: &mut BTreeMap<StepId, BTreeSet<StepId>>,
) -> Vec<[String; 2]> {
    let by_id: std::collections::HashMap<&str, &ValidatedPlanNode> = validated
        .nodes()
        .iter()
        .map(|n| (n.id().as_str(), n))
        .collect();
    let mut last_write: Option<StepId> = None;
    let mut added = Vec::new();
    for id_str in validated.topological_order() {
        let Some(node) = by_id.get(id_str.as_str()) else {
            continue;
        };
        match node.effect_class() {
            EffectClass::Write | EffectClass::SideEffect => {
                let Ok(id) = StepId::new(id_str.as_str().to_string()) else {
                    continue;
                };
                if let Some(ref prev) = last_write {
                    if deps.entry(id.clone()).or_default().insert(prev.clone()) {
                        added.push([prev.as_str().to_string(), id.as_str().to_string()]);
                    }
                }
                last_write = Some(id);
            }
            _ => last_write = None,
        }
    }
    added
}

fn primary_predecessor(node: &ValidatedPlanNode) -> Option<String> {
    match node {
        ValidatedPlanNode::Compute(n) => Some(n.compute.source.clone()),
        ValidatedPlanNode::Derive(n) => Some(n.source.as_str().to_string()),
        ValidatedPlanNode::ForEach(n) => Some(n.source.as_str().to_string()),
        ValidatedPlanNode::RelationTraversal(n) => Some(n.relation.source.as_str().to_string()),
        ValidatedPlanNode::Surface(n) => n.uses_result.first().map(|u| u.node.clone()),
        ValidatedPlanNode::Data(_) => None,
    }
}

/// Validated trace topology from a dry-run evaluation (single builder for trace + MCP meta).
pub fn trace_comp_wire_from_dry(dry: &DryPlasmPlanEvaluation) -> TraceCompWire {
    trace_comp_wire_from_artifact(dry.artifact(), Some(dry.graph_summary.clone()))
}

/// Validated trace topology from a comp artifact (optional dry-run summary).
pub fn trace_comp_wire_from_artifact(
    artifact: &PlasmCompArtifact,
    summary: Option<serde_json::Value>,
) -> TraceCompWire {
    TraceCompWire {
        comp: artifact.comp.clone(),
        summary,
        returns: render_return_lines_from_comp(&artifact.comp.return_),
    }
}

/// Wire JSON for MCP/HTTP `_meta.plasm.comp` (greenfield).
pub fn plasm_comp_wire_json(
    artifact: &PlasmCompArtifact,
    summary: Option<&serde_json::Value>,
) -> serde_json::Value {
    trace_comp_wire_from_artifact(artifact, summary.cloned()).to_json_value()
}

fn render_return_lines_from_comp(ret: &PlasmReturn) -> Vec<String> {
    match ret {
        PlasmReturn::Step { step } => vec![step.as_str().to_string()],
        PlasmReturn::Parallel { steps } => steps.iter().map(|s| s.as_str().to_string()).collect(),
    }
}

/// Canonical semantic subset for plan commit hashing.
pub fn plasm_comp_commit_canonical(comp: &PlasmComp) -> serde_json::Value {
    plasm_core::plasm_comp_commit_canonical(comp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::parse_and_validate_plan_json;

    #[test]
    fn comp_from_minimal_query_plan() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "t",
            "nodes": [{
                "id": "items",
                "kind": "query",
                "effect_class": "read",
                "result_shape": "list",
                "expr": "Item{}",
                "qualified_entity": { "entry_id": "matrix", "entity": "Item" },
                "ir": { "expr": { "op": "query", "entity": "Item" } },
                "depends_on": [],
                "uses_result": []
            }],
            "return": { "kind": "node", "node": "items" }
        });
        let validated = parse_and_validate_plan_json(&plan).expect("validate");
        let artifact = plasm_comp_from_validated(&validated);
        assert!(artifact.comp.validate().is_ok());
        assert_eq!(artifact.comp.bind.topo.len(), 1);
    }
}
