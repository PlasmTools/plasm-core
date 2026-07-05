//! Lowered DAG node → plan JSON.

use super::super::prelude::*;
use super::super::types::{DagNode, DagNodeSource, PlanNodeEmitter};
use super::template_uses::relation_plan_uses_result;

pub(in crate::plasm_dag) fn node_to_json(node: &DagNode) -> Result<serde_json::Value, String> {
    node.source.emit_plan_json(node)
}

impl PlanNodeEmitter for DagNodeSource {
    fn emit_plan_json(&self, node: &DagNode) -> Result<serde_json::Value, String> {
        emit_plan_json_for_source(self, node)
    }
}
pub(in crate::plasm_dag) fn emit_plan_json_for_source(
    source: &DagNodeSource,
    node: &DagNode,
) -> Result<serde_json::Value, String> {
    match source {
        DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result,
        } => {
            let ir = if uses_result.is_empty() {
                json!({
                    "expr": parsed.expr,
                    "projection": parsed.projection,
                })
            } else {
                expr_template_json(parsed, uses_result)?
            };
            let mut obj = json!({
                "id": node.id,
                "kind": kind,
                "expr": node.expr,
                "effect_class": effect_class,
                "result_shape": result_shape,
                "projection": parsed.projection.clone().unwrap_or_default(),
                "predicates": [],
                "depends_on": uses_result.iter().filter_map(|u| u.get("node").and_then(|v| v.as_str()).map(str::to_string)).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
                "uses_result": uses_result,
            });
            if matches!(result_shape, crate::plasm_plan::ResultShape::Page) {
                obj["qualified_entity"] = serde_json::Value::Null;
            } else {
                obj["qualified_entity"] = json!(qualified_entity);
            }
            if uses_result.is_empty() {
                obj["ir"] = ir;
            } else {
                obj["ir_template"] = ir;
            }
            if let Some(n) = node.page_size {
                obj["page_size"] = json!(n);
            }
            Ok(obj)
        }
        DagNodeSource::RelationTraversal {
            source_label,
            parsed,
            plan_relation,
            qualified_entity,
            effect_class,
            result_shape,
            ..
        } => {
            let mut obj = json!({
                "id": node.id,
                "kind": PlanNodeKind::Relation,
                "qualified_entity": qualified_entity,
                "effect_class": effect_class,
                "result_shape": result_shape,
                "projection": parsed.projection.clone().unwrap_or_default(),
                "predicates": [],
                "relation": plan_relation,
                "depends_on": [source_label],
                "uses_result": relation_plan_uses_result(source_label, parsed),
            });
            if let Some(n) = node.page_size {
                obj["page_size"] = json!(n);
            }
            Ok(obj)
        }
        DagNodeSource::Data(value) => Ok(json!({
            "id": node.id,
            "kind": "data",
            "effect_class": "artifact_read",
            "result_shape": "artifact",
            "data": value,
            "depends_on": [],
            "uses_result": [],
        })),
        DagNodeSource::Compute {
            source,
            op,
            schema,
            collection_alias,
        } => {
            let mut compute = json!({
                "source": source,
                "op": op,
                "schema": schema,
                "page_size": node.page_size,
            });
            if let Some(alias) = collection_alias {
                compute["collection_alias"] = json!(alias);
            }
            let (depends_on, uses_result) = match op {
                ComputeOp::Render {
                    render_bindings, ..
                } => render_plan_graph_edges(source, render_bindings),
                _ => (
                    vec![source.clone()],
                    vec![json!({ "node": source, "as": "source" })],
                ),
            };
            Ok(json!({
                "id": node.id,
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": if matches!(op, ComputeOp::Render { .. }) { "single" } else { "list" },
                "compute": compute,
                "depends_on": depends_on,
                "uses_result": uses_result,
            }))
        }
        DagNodeSource::Derive {
            source,
            value,
            inputs,
        } => {
            let mut depends = vec![source.clone()];
            for input in inputs {
                if let Some(n) = input.get("node").and_then(|v| v.as_str()) {
                    if !depends.iter().any(|d| d == n) {
                        depends.push(n.to_string());
                    }
                }
            }
            Ok(json!({
                "id": node.id,
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": depends,
                "uses_result": std::iter::once(json!({ "node": source, "as": "_" })).chain(inputs.iter().map(|input| {
                    json!({
                        "node": input.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                        "as": input.get("alias").and_then(|v| v.as_str()).unwrap_or_default(),
                    })
                })).collect::<Vec<_>>(),
                "derive_template": {
                    "kind": "map",
                    "source": source,
                    "item_binding": "_",
                    "inputs": inputs,
                    "value": value,
                }
            }))
        }
        DagNodeSource::ForEach {
            source,
            parsed_template,
            display_expr,
            effect_kind,
            qualified_entity,
            uses_result,
        } => {
            let mut depends = vec![source.clone()];
            for input in uses_result {
                if let Some(n) = input.get("node").and_then(|v| v.as_str()) {
                    if !depends.iter().any(|d| d == n) {
                        depends.push(n.to_string());
                    }
                }
            }
            Ok(json!({
                "id": node.id,
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": source,
                "item_binding": "_",
                "depends_on": depends,
                "uses_result": std::iter::once(json!({ "node": source, "as": "_" })).chain(uses_result.iter().cloned()).collect::<Vec<_>>(),
                "effect_template": {
                    "kind": effect_kind,
                    "qualified_entity": qualified_entity,
                    "expr_template": display_expr,
                    "ir_template": parsed_template,
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "projection": [],
                    "input_bindings": [],
                }
            }))
        }
    }
}
pub(in crate::plasm_dag) fn expr_template_json(
    parsed: &plasm_core::expr_parser::ParsedExpr,
    uses: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let value = serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?;
    let mut obj = serde_json::Map::new();
    obj.insert("expr".to_string(), value);
    if let Some(proj) = parsed.projection.clone() {
        obj.insert(
            "projection".to_string(),
            serde_json::to_value(proj).map_err(|e| e.to_string())?,
        );
    }
    obj.insert(
        "input_bindings".to_string(),
        serde_json::Value::Array(
            uses
                .iter()
                .map(|u| {
                    json!({
                        "from": u.get("as").and_then(|v| v.as_str()).unwrap_or_default(),
                        "to": u.get("as").and_then(|v| v.as_str()).unwrap_or_default(),
                    })
                })
                .collect(),
        ),
    );
    Ok(serde_json::Value::Object(obj))
}
