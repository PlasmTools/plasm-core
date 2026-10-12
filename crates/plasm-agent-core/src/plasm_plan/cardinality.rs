//! Plan-IR static row-cardinality analysis.
//!
//! **One vocabulary:** analysis returns [`RowCardinalityProof`] — the same lattice the DAG binding
//! layer uses — rather than a parallel `CardinalityAnalysis` enum. Limit≤1 / `.singleton()` is
//! [`RowCardinalityProof::BoundedSingleton`] (never silent [`RowCardinalityProof::StaticSingleton`]).
//!
//! Downstream gates must go through the binding mappers:
//! - Auto broadcast → [`RowCardinalityProof::try_auto_broadcast_input_proof`]
//! - Relation `Single` / D1 static check → [`RowCardinalityProof::is_static_singleton`]
//! - Relation source labels → [`RowCardinalityProof::to_relation_source_cardinality`]
//! - Explicit `InputCardinality::Singleton` → always
//!   [`super::InputCardinalityProof::RuntimeCheckedSingleton`] (validate layer; no silent Static)
//!
//! Plan-IR walks that cannot distinguish proven plural from unknown collapse both to
//! [`RowCardinalityProof::StaticPlural`] (a “not singleton” judgment for Auto / `Single` gates).

use std::collections::{HashMap, HashSet};

use super::{
    Plan, PlanNodeKind, PlanValue, RelationCardinality, RelationSourceCardinality,
    ValidatedPlanNode, ValidatedPlanState,
};
use crate::program_binding::RowCardinalityProof;

use super::compute_transfer::{compute_cardinality_transfer, map_body_cardinality_transfer};

/// Evaluate lazy cardinality transfers using a heap continuation stack. Transfers
/// request at most one source; source-independent operations never traverse it.
fn analyze_cardinality(
    root: &str,
    mut classify: impl FnMut(&str, &mut dyn FnMut(&str) -> RowCardinalityProof) -> RowCardinalityProof,
) -> RowCardinalityProof {
    let mut memo = HashMap::new();
    let mut active = HashSet::new();
    let mut pending = vec![root.to_owned()];
    while let Some(node) = pending.last().cloned() {
        active.insert(node.clone());
        let mut missing = None;
        let proof = classify(&node, &mut |source| {
            if let Some(proof) = memo.get(source) {
                *proof
            } else {
                if !active.contains(source) {
                    missing = Some(source.to_owned());
                }
                // Unknown/cyclic inputs do not establish a singleton proof.
                // Full dependency validation reports cycles independently.
                RowCardinalityProof::StaticPlural
            }
        });
        if let Some(source) = missing {
            pending.push(source);
        } else {
            memo.insert(node.clone(), proof);
            active.remove(&node);
            pending.pop();
        }
    }
    memo[root]
}

/// Walk an unvalidated plan DAG and classify the row cardinality of `node_id`.
pub(super) fn analyze_static_cardinality(
    plan: &Plan,
    by_id: &HashMap<String, usize>,
    node_id: &str,
) -> RowCardinalityProof {
    fn classify(
        plan: &Plan,
        by_id: &HashMap<String, usize>,
        node_id: &str,
        source_proof: &mut dyn FnMut(&str) -> RowCardinalityProof,
    ) -> RowCardinalityProof {
        let Some(index) = by_id.get(node_id).copied() else {
            return RowCardinalityProof::StaticPlural;
        };
        let node = &plan.nodes[index];
        match node.kind {
            kind if surface_is_singleton(kind, node.result_shape) => {
                RowCardinalityProof::StaticSingleton
            }
            PlanNodeKind::Data => match &node.data {
                Some(PlanValue::Array { items }) if items.len() == 1 => {
                    RowCardinalityProof::StaticSingleton
                }
                Some(PlanValue::Literal { value })
                    if value.as_array().is_none_or(|items| items.len() == 1) =>
                {
                    RowCardinalityProof::StaticSingleton
                }
                Some(
                    PlanValue::Array { .. }
                    | PlanValue::Literal { .. }
                    | PlanValue::EntityRefKey { .. },
                )
                | None => RowCardinalityProof::StaticPlural,
                Some(_) => RowCardinalityProof::StaticSingleton,
            },
            PlanNodeKind::Derive => node
                .derive_template
                .as_ref()
                .and_then(|t| t.source.as_deref())
                .map(source_proof)
                .unwrap_or(RowCardinalityProof::StaticPlural),
            PlanNodeKind::Compute => node
                .compute
                .as_ref()
                .map(|compute| {
                    compute_cardinality_transfer(&compute.op, || source_proof(&compute.source))
                })
                .unwrap_or(RowCardinalityProof::StaticPlural),
            PlanNodeKind::MapBody => node
                .map_body
                .as_ref()
                .map(|body| {
                    map_body_cardinality_transfer(&body.output, || {
                        source_proof(body.parent.source.as_str())
                    })
                })
                .unwrap_or(RowCardinalityProof::StaticPlural),
            PlanNodeKind::Relation => node
                .relation
                .as_ref()
                .map(
                    |relation| match (relation.cardinality, relation.source_cardinality) {
                        (RelationCardinality::One, RelationSourceCardinality::Single) => {
                            source_proof(&relation.source)
                        }
                        (
                            RelationCardinality::One,
                            RelationSourceCardinality::RuntimeCheckedSingleton,
                        ) => RowCardinalityProof::StaticPlural,
                        (RelationCardinality::Many, _)
                        | (RelationCardinality::One, RelationSourceCardinality::Many) => {
                            RowCardinalityProof::StaticPlural
                        }
                    },
                )
                .unwrap_or(RowCardinalityProof::StaticPlural),
            _ => RowCardinalityProof::StaticPlural,
        }
    }
    analyze_cardinality(node_id, |id, source| classify(plan, by_id, id, source))
}

/// Whether a validated plan node is provably a single row (D1 effect-cardinality typing).
///
/// [`RowCardinalityProof::BoundedSingleton`] (Limit≤1) is **not** static — returns false.
pub(crate) fn validated_source_is_static_singleton(
    plan: &Plan<ValidatedPlanState>,
    source_id: &str,
) -> bool {
    let by_id: HashMap<String, usize> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().as_str().to_string(), i))
        .collect();
    validated_analyze_static_cardinality(&plan.nodes, &by_id, source_id).is_static_singleton()
}

fn validated_analyze_static_cardinality(
    nodes: &[ValidatedPlanNode],
    by_id: &HashMap<String, usize>,
    node_id: &str,
) -> RowCardinalityProof {
    fn classify(
        nodes: &[ValidatedPlanNode],
        by_id: &HashMap<String, usize>,
        node_id: &str,
        source_proof: &mut dyn FnMut(&str) -> RowCardinalityProof,
    ) -> RowCardinalityProof {
        let Some(index) = by_id.get(node_id).copied() else {
            return RowCardinalityProof::StaticPlural;
        };
        let node = &nodes[index];
        match node {
            ValidatedPlanNode::Capture(c) => {
                if c.singleton {
                    RowCardinalityProof::StaticSingleton
                } else {
                    RowCardinalityProof::StaticPlural
                }
            }
            ValidatedPlanNode::Surface(s) if surface_is_singleton(s.kind, s.result_shape) => {
                RowCardinalityProof::StaticSingleton
            }
            ValidatedPlanNode::Data(d) => match &d.data {
                PlanValue::Array { items } if items.len() == 1 => {
                    RowCardinalityProof::StaticSingleton
                }
                PlanValue::Literal { value }
                    if value.as_array().is_none_or(|items| items.len() == 1) =>
                {
                    RowCardinalityProof::StaticSingleton
                }
                PlanValue::Array { .. }
                | PlanValue::Literal { .. }
                | PlanValue::EntityRefKey { .. } => RowCardinalityProof::StaticPlural,
                _ => RowCardinalityProof::StaticSingleton,
            },
            ValidatedPlanNode::Derive(d) => source_proof(d.source.as_str()),
            ValidatedPlanNode::Compute(c) => compute_cardinality_transfer(&c.compute.op, || {
                source_proof(c.compute.source.as_str())
            }),
            ValidatedPlanNode::MapBody(map) => {
                map_body_cardinality_transfer(&map.body.output, || {
                    source_proof(map.body.parent.source.as_str())
                })
            }
            ValidatedPlanNode::RelationTraversal(r) => {
                match (r.relation.cardinality, r.relation.source_cardinality) {
                    (RelationCardinality::One, RelationSourceCardinality::Single) => {
                        source_proof(r.relation.source.as_str())
                    }
                    (
                        RelationCardinality::One,
                        RelationSourceCardinality::RuntimeCheckedSingleton,
                    ) => RowCardinalityProof::StaticPlural,
                    (RelationCardinality::Many, _)
                    | (RelationCardinality::One, RelationSourceCardinality::Many) => {
                        RowCardinalityProof::StaticPlural
                    }
                }
            }
            _ => RowCardinalityProof::StaticPlural,
        }
    }
    analyze_cardinality(node_id, |id, source| classify(nodes, by_id, id, source))
}

/// Catalog entity-returning mutations have the same singleton result contract
/// as Get. A list read cannot acquire this proof by changing its wire shape.
fn surface_is_singleton(kind: PlanNodeKind, shape: super::ResultShape) -> bool {
    kind == PlanNodeKind::Get
        || (matches!(
            kind,
            PlanNodeKind::Create
                | PlanNodeKind::Update
                | PlanNodeKind::Delete
                | PlanNodeKind::Action
        ) && shape == super::ResultShape::MutationResult)
}

/// Limit≤1 → BoundedSingleton; `from_plural_source` matches DAG binding_contract (StaticPlural /
/// RuntimeChecked parents only).
/// Reuse the ordinary cardinality lattice when admitting an enclosing scope port.
pub(crate) fn scoped_capture_permits_singleton(nodes: &[ValidatedPlanNode], source: &str) -> bool {
    let Ok(id) = super::PlanNodeId::new(source) else {
        return false;
    };
    let source = id.as_str();
    let by_id = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().to_string(), i))
        .collect();
    validated_analyze_static_cardinality(nodes, &by_id, source).permits_scalar_field_extract()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::parse_plan_value;
    use crate::program_binding::BoundedSingletonKind;

    #[test]
    fn appended_cardinality_dependencies_do_not_grow_native_stack() {
        let count = 32_768;
        let mut plan = parse_plan_value(&serde_json::json!({
            "version": 1, "kind": "program",
            "nodes": [{"id": "seed", "kind": "data", "effect_class": "artifact_read",
                "result_shape": "artifact", "data": {"kind": "literal", "value": true}}],
            "return": {"kind": "node", "node": "n0"}
        }))
        .unwrap();
        let seed = plan.nodes[0].clone();
        plan.nodes.clear();
        for index in 0..count {
            let mut node = seed.clone();
            node.id = format!("n{index}");
            if index + 1 < count {
                node.kind = PlanNodeKind::Derive;
                node.data = None;
                node.derive_template = Some(super::super::DeriveTemplate {
                    kind: super::super::DeriveKind::Cell,
                    source: Some(format!("n{}", index + 1)),
                    item_binding: Some("item".into()),
                    inputs: vec![],
                    value: seed.data.clone().unwrap(),
                });
            }
            plan.nodes.push(node);
        }
        let by_id = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.clone(), index))
            .collect();
        assert_eq!(
            analyze_static_cardinality(&plan, &by_id, "n0"),
            RowCardinalityProof::StaticSingleton
        );
        let nodes: Vec<_> = plan
            .nodes
            .iter()
            .map(|node| {
                let id = super::super::PlanNodeId::new(&node.id).unwrap();
                if let Some(template) = &node.derive_template {
                    ValidatedPlanNode::Derive(super::super::ValidatedDeriveNode {
                        kind: template.kind,
                        id,
                        effect_class: node.effect_class,
                        result_shape: node.result_shape,
                        source: super::super::PlanNodeId::new(template.source.as_ref().unwrap())
                            .unwrap(),
                        item_binding: super::super::BindingName::new("item").unwrap(),
                        inputs: vec![],
                        value: template.value.clone(),
                        depends_on: vec![],
                        uses_result: vec![],
                    })
                } else {
                    ValidatedPlanNode::Data(super::super::ValidatedDataNode {
                        id,
                        effect_class: node.effect_class,
                        result_shape: node.result_shape,
                        data: node.data.clone().unwrap(),
                        depends_on: vec![],
                        uses_result: vec![],
                    })
                }
            })
            .collect();
        assert_eq!(
            validated_analyze_static_cardinality(&nodes, &by_id, "n0"),
            RowCardinalityProof::StaticSingleton
        );
        plan.nodes[count - 1].kind = PlanNodeKind::Derive;
        plan.nodes[count - 1].derive_template = plan.nodes[0].derive_template.clone();
        assert_eq!(
            analyze_static_cardinality(&plan, &by_id, "n0"),
            RowCardinalityProof::StaticPlural
        );
    }

    #[test]
    fn cardinality_does_not_visit_unused_sources() {
        let mut visits = Vec::new();
        let proof = analyze_cardinality("root", |node, _source| {
            visits.push(node.to_owned());
            RowCardinalityProof::StaticSingleton
        });
        assert_eq!(proof, RowCardinalityProof::StaticSingleton);
        assert_eq!(visits, ["root"]);
    }

    proptest::proptest! {
        #[test]
        fn appended_proof_transfers_preserve_proof_with_linear_work(
            count in 1usize..512,
            singleton in proptest::bool::ANY,
            cyclic in proptest::bool::ANY,
        ) {
            let mut visits = 0usize;
            let terminal = if singleton {
                RowCardinalityProof::StaticSingleton
            } else {
                RowCardinalityProof::StaticPlural
            };
            let proof = analyze_cardinality("0", |node, source| {
                visits += 1;
                let index: usize = node.parse().unwrap();
                if index + 1 < count {
                    source(&(index + 1).to_string())
                } else if cyclic {
                    source("0")
                } else {
                    terminal
                }
            });
            proptest::prop_assert_eq!(proof, if cyclic {
                RowCardinalityProof::StaticPlural
            } else {
                terminal
            });
            proptest::prop_assert_eq!(visits, count * 2 - 1);
        }
    }

    fn render_plan(columns: serde_json::Value, template: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "name": "bolt" }] }
                },
                {
                    "id": "doc",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "rows",
                        "op": { "kind": "render", "columns": columns, "template": template },
                        "schema": {
                            "entity": "PlanRender",
                            "fields": [{ "name": "content", "value_kind": "string" }]
                        }
                    },
                    "depends_on": ["rows"],
                    "uses_result": [{ "node": "rows", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "doc" }
        })
    }

    #[test]
    fn analyze_static_cardinality_limit_one_is_bounded_not_static() {
        let plan = parse_plan_value(&serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "limit-one-card",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "state": "open" }, { "state": "closed" }] }
                },
                {
                    "id": "one",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "rows",
                        "op": { "kind": "limit", "count": 1 },
                        "schema": { "fields": [{ "name": "state", "value_kind": "string", "source": ["state"] }] },
                        "page_size": 1
                    }
                }
            ],
            "return": { "kind": "node", "node": "one" }
        }))
        .expect("parse");
        let by_id = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.id.clone(), idx))
            .collect::<HashMap<_, _>>();
        let proof = analyze_static_cardinality(&plan, &by_id, "one");
        assert_eq!(
            proof,
            RowCardinalityProof::BoundedSingleton {
                kind: BoundedSingletonKind::LimitOne,
                from_plural_source: true,
            }
        );
        assert!(!proof.is_static_singleton());
        assert_eq!(
            proof.try_auto_broadcast_input_proof(),
            Some(crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton)
        );
    }

    #[test]
    fn analyze_static_cardinality_render_is_static_singleton() {
        let plan = parse_plan_value(&render_plan(
            serde_json::json!(["name"]),
            serde_json::json!("{{ rows }}"),
        ))
        .expect("parse");
        let by_id = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.id.clone(), idx))
            .collect::<HashMap<_, _>>();

        let proof = analyze_static_cardinality(&plan, &by_id, "doc");
        assert!(proof.is_static_singleton());
        assert_eq!(
            proof.try_auto_broadcast_input_proof(),
            Some(crate::plasm_plan::InputCardinalityProof::StaticSingleton)
        );
    }

    #[test]
    fn render_of_plural_source_is_plural_in_both_plan_states() {
        let mut value = render_plan(serde_json::json!(["name"]), serde_json::json!("{{ name }}"));
        value["nodes"][0]["data"]["value"] =
            serde_json::json!([{ "name": "bolt" }, { "name": "nut" }]);
        let plan = parse_plan_value(&value).expect("parse");
        let by_id = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.id.clone(), idx))
            .collect::<HashMap<_, _>>();
        assert_eq!(
            analyze_static_cardinality(&plan, &by_id, "doc"),
            RowCardinalityProof::StaticPlural
        );

        let validated = super::super::validate_plan_artifact(&plan).expect("validate");
        assert!(!validated_source_is_static_singleton(
            validated.artifact(),
            "doc"
        ));
    }
}
