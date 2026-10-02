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

use std::collections::HashMap;

use super::{
    Plan, PlanNodeKind, PlanValue, RelationCardinality, RelationSourceCardinality,
    ValidatedPlanNode, ValidatedPlanState,
};
use crate::program_binding::RowCardinalityProof;

use super::compute_transfer::compute_cardinality_transfer;

/// Walk an unvalidated plan DAG and classify the row cardinality of `node_id`.
pub(super) fn analyze_static_cardinality(
    plan: &Plan,
    by_id: &HashMap<String, usize>,
    node_id: &str,
) -> RowCardinalityProof {
    fn inner(
        plan: &Plan,
        by_id: &HashMap<String, usize>,
        node_id: &str,
        memo: &mut HashMap<String, RowCardinalityProof>,
    ) -> RowCardinalityProof {
        if let Some(v) = memo.get(node_id) {
            return *v;
        }
        let Some(index) = by_id.get(node_id).copied() else {
            return RowCardinalityProof::StaticPlural;
        };
        let node = &plan.nodes[index];
        let proof = match node.kind {
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
                .map(|source| inner(plan, by_id, source, memo))
                .unwrap_or(RowCardinalityProof::StaticPlural),
            PlanNodeKind::Compute => node
                .compute
                .as_ref()
                .map(|compute| {
                    compute_cardinality_transfer(&compute.op, || {
                        inner(plan, by_id, &compute.source, memo)
                    })
                })
                .unwrap_or(RowCardinalityProof::StaticPlural),
            PlanNodeKind::Relation => node
                .relation
                .as_ref()
                .map(
                    |relation| match (relation.cardinality, relation.source_cardinality) {
                        (RelationCardinality::One, RelationSourceCardinality::Single) => {
                            inner(plan, by_id, &relation.source, memo)
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
        };
        memo.insert(node_id.to_string(), proof);
        proof
    }
    inner(plan, by_id, node_id, &mut HashMap::new())
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
    validated_analyze_static_cardinality(plan, &by_id, source_id).is_static_singleton()
}

fn validated_analyze_static_cardinality(
    plan: &Plan<ValidatedPlanState>,
    by_id: &HashMap<String, usize>,
    node_id: &str,
) -> RowCardinalityProof {
    fn inner(
        plan: &Plan<ValidatedPlanState>,
        by_id: &HashMap<String, usize>,
        node_id: &str,
        memo: &mut HashMap<String, RowCardinalityProof>,
    ) -> RowCardinalityProof {
        if let Some(v) = memo.get(node_id) {
            return *v;
        }
        let Some(index) = by_id.get(node_id).copied() else {
            return RowCardinalityProof::StaticPlural;
        };
        let node = &plan.nodes[index];
        let proof = match node {
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
            ValidatedPlanNode::Derive(d) => inner(plan, by_id, d.source.as_str(), memo),
            ValidatedPlanNode::Compute(c) => compute_cardinality_transfer(&c.compute.op, || {
                inner(plan, by_id, c.compute.source.as_str(), memo)
            }),
            ValidatedPlanNode::RelationTraversal(r) => {
                match (r.relation.cardinality, r.relation.source_cardinality) {
                    (RelationCardinality::One, RelationSourceCardinality::Single) => {
                        inner(plan, by_id, r.relation.source.as_str(), memo)
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
        };
        memo.insert(node_id.to_string(), proof);
        proof
    }
    inner(plan, by_id, node_id, &mut HashMap::new())
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
    let plan = Plan::new_program(
        plasm_core::plasm_monad::PLASM_COMP_WIRE_VERSION,
        None,
        nodes.to_vec(),
        super::ValidatedPlanReturn::Node(id),
        Default::default(),
    );
    let by_id = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().to_string(), i))
        .collect();
    validated_analyze_static_cardinality(&plan, &by_id, source).permits_scalar_field_extract()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::parse_plan_value;
    use crate::program_binding::BoundedSingletonKind;

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
