//! Plan node dependency graph and return-reachability (shared by dry-run review + read bounds).

use std::collections::HashSet;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    EffectClass, Plan, ValidatedPlanNode, ValidatedPlanReturn, ValidatedPlanState,
};

pub(crate) fn push_unique(out: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        if !out.iter().any(|seen| seen == &value) {
            out.push(value);
        }
    }
}

#[must_use]
pub fn node_dependencies(node: &ValidatedPlanNode) -> Vec<String> {
    let mut out = Vec::new();
    push_unique(
        &mut out,
        node.depends_on().iter().map(|id| id.as_str().to_string()),
    );
    push_unique(&mut out, node.uses_result().iter().map(|u| u.node.clone()));
    match node {
        ValidatedPlanNode::Derive(n) => {
            push_unique(&mut out, std::iter::once(n.source.as_str().to_string()));
            push_unique(
                &mut out,
                n.inputs.iter().map(|input| input.node.as_str().to_string()),
            );
        }
        ValidatedPlanNode::Compute(n) => {
            push_unique(&mut out, std::iter::once(n.compute.source.clone()));
        }
        ValidatedPlanNode::ForEach(n) => {
            push_unique(&mut out, std::iter::once(n.source.as_str().to_string()));
        }
        ValidatedPlanNode::RelationTraversal(n) => {
            push_unique(
                &mut out,
                std::iter::once(n.relation.source.as_str().to_string()),
            );
        }
        _ => {}
    }
    out
}

/// Node ids on paths from program return backward through [`node_dependencies`].
#[must_use]
pub fn nodes_reachable_from_return(plan: &Plan<ValidatedPlanState>) -> HashSet<String> {
    let mut reachable = HashSet::new();
    let mut stack = match &plan.return_value {
        ValidatedPlanReturn::Node(id) => vec![id.as_str().to_string()],
        ValidatedPlanReturn::Parallel { parallel } => {
            parallel.iter().map(|id| id.as_str().to_string()).collect()
        }
    };
    while let Some(id) = stack.pop() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        let Some(node) = plan.nodes.iter().find(|n| n.id().as_str() == id) else {
            continue;
        };
        for upstream in node_dependencies(node) {
            if !reachable.contains(upstream.as_str()) {
                stack.push(upstream);
            }
        }
    }
    reachable
}

/// Bindings whose nodes would execute but do not feed downstream holes or the program return.
#[must_use]
pub(crate) fn unused_binding_hints(plan: &Plan<ValidatedPlanState>) -> Vec<String> {
    let needed = nodes_reachable_from_return(plan);
    let mut unused = Vec::new();
    for n in &plan.nodes {
        let id = n.id().as_str();
        if needed.contains(id) {
            continue;
        }
        if matches!(n, ValidatedPlanNode::Surface(s) if s.effect_class == EffectClass::SideEffect) {
            continue;
        }
        if !matches!(
            n,
            ValidatedPlanNode::Surface(_)
                | ValidatedPlanNode::Compute(_)
                | ValidatedPlanNode::Derive(_)
                | ValidatedPlanNode::ForEach(_)
                | ValidatedPlanNode::RelationTraversal(_)
        ) {
            continue;
        }
        if matches!(n, ValidatedPlanNode::Surface(s) if s.effect_class == EffectClass::ArtifactRead)
        {
            continue;
        }
        unused.push(id.to_string());
    }
    unused.sort();
    unused
}

pub(crate) fn unused_seed_hints(
    es: &ExecuteSession,
    plan: &Plan<ValidatedPlanState>,
) -> Vec<String> {
    let used = crate::plan_prepare::collect_plan_entity_names(plan);
    es.entities
        .iter()
        .filter(|e| !used.contains(e.as_str()))
        .map(|e| {
            format!(
                "{}:{}",
                crate::catalog_ownership::entry_id_for_entity_trace(es, e.as_str()),
                e
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_reachable_from_return_includes_uses_result_upstream() {
        let plan = serde_json::from_value(serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "uses-result",
            "nodes": [
                {
                    "id": "src",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "out",
                    "kind": "derive",
                    "effect_class": "read",
                    "result_shape": "single",
                    "derive_template": {
                        "kind": "map",
                        "source": "src",
                        "item_binding": "row",
                        "value": { "kind": "object", "fields": { "x": { "kind": "literal", "value": 1 } } }
                    },
                    "depends_on": ["src"],
                    "uses_result": [{ "node": "src", "as": "row" }]
                }
            ],
            "return": { "kind": "node", "node": "out" }
        }))
        .expect("plan json");
        let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        let artifact = validated.artifact();
        let reachable = nodes_reachable_from_return(artifact);
        assert!(reachable.contains("src"));
        assert!(reachable.contains("out"));
    }
}
