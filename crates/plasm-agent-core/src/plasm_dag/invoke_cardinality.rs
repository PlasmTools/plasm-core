//! Compile-time gate: plural query-all field extracts must not fill scalar invoke params.

use super::binding_contract::binding_contract;
use super::prelude::*;
use super::schema_validate::cgs_for_qualified_entity;
use super::types::{CompileState, DagNodeSource};
use plasm_core::{plp, FieldType, PlasmInputRef, Predicate, Value};

/// Reject `username=account.account_name` when `account` is bare query-all (plural → array at live).
pub(in crate::plasm_dag) fn validate_invoke_scalar_field_refs(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    expr: &Expr,
) -> Result<(), String> {
    let Expr::Invoke(inv) = expr else {
        return Ok(());
    };
    let Some(input) = &inv.input else {
        return Ok(());
    };
    let qe = infer_invoke_qualified_entity(session, inv);
    let cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let Some(cap) = cgs.get_capability(inv.capability.as_str()) else {
        return Ok(());
    };
    let fields: Vec<_> = cap.invocation_object_fields().collect();
    if fields.is_empty() {
        return Ok(());
    }
    let value = input.to_value();
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    for (param, val) in obj {
        let Some(field) = fields.iter().find(|f| f.name == *param) else {
            continue;
        };
        let Ok(nv) = field.named_value(cgs.as_ref()) else {
            continue;
        };
        if param_accepts_plural_column_projection(&nv.field_type) {
            continue;
        }
        reject_bare_plural_field_refs(state, node_id, param, val)?;
    }
    if let Some(path_vars) = &inv.path_vars {
        for (param, val) in path_vars {
            let Some(field) = fields.iter().find(|f| f.name == *param) else {
                continue;
            };
            let Ok(nv) = field.named_value(cgs.as_ref()) else {
                continue;
            };
            if param_accepts_plural_column_projection(&nv.field_type) {
                continue;
            }
            reject_bare_plural_field_refs(state, node_id, param, val)?;
        }
    }
    Ok(())
}

fn infer_invoke_qualified_entity(
    session: &ExecuteSession,
    inv: &plasm_core::InvokeExpr,
) -> QualifiedEntityKey {
    let entry = inv
        .catalog_entry_id
        .as_deref()
        .unwrap_or(session.entry_id.as_str());
    QualifiedEntityKey {
        entry_id: entry.to_string(),
        entity: inv.target.entity_type.as_str().to_string(),
    }
}

fn param_accepts_plural_column_projection(ft: &FieldType) -> bool {
    matches!(
        ft,
        FieldType::Array | FieldType::Json | FieldType::MultiSelect
    )
}

fn reject_bare_plural_field_refs(
    state: &CompileState<'_>,
    node_id: &str,
    param: &str,
    value: &Value,
) -> Result<(), String> {
    match value {
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) if !path.is_empty() => {
            if binding_is_bare_query_all_plural(state, node) {
                return Err(plp::plp4_program(
                    node_id,
                    format!(
                        "param `{param}` expects a scalar, but `{node}.{}` projects a field from bare query-all (plural). Narrow with `e#{{…}}` / `.filter{{…}}` then `.limit(1)` / `.singleton()`, or use a get / nullary `e#.m#()` row before passing `.wire` into a string param",
                        path.join(".")
                    ),
                ));
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_bare_plural_field_refs(state, node_id, param, item)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for v in fields.values() {
                reject_bare_plural_field_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        Value::UnionCtor { ctor_fields, .. } => {
            for v in ctor_fields.values() {
                reject_bare_plural_field_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// True when `label` ultimately comes from an unfiltered Query/Search surface (query-all).
fn binding_is_bare_query_all_plural(state: &CompileState<'_>, label: &str) -> bool {
    let Some(contract) = binding_contract(state, label) else {
        return false;
    };
    if !matches!(
        contract.row_cardinality,
        RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked
    ) {
        return false;
    }
    ultimate_surface_is_bare_list_producer(state, label)
}

fn ultimate_surface_is_bare_list_producer(state: &CompileState<'_>, label: &str) -> bool {
    let Some(node) = state.get(label) else {
        return false;
    };
    match &node.source {
        DagNodeSource::Surface {
            parsed,
            kind: PlanNodeKind::Query | PlanNodeKind::Search,
            ..
        } => match &parsed.expr {
            Expr::Query(q) => predicate_is_absent_or_true(q.predicate.as_ref()),
            _ => false,
        },
        DagNodeSource::Surface { .. } => false,
        DagNodeSource::Compute { source, op, .. } => match op {
            ComputeOp::Filter { .. }
            | ComputeOp::Aggregate { .. }
            | ComputeOp::Render { .. }
            | ComputeOp::GroupBy { .. } => false,
            ComputeOp::Limit { count } if *count <= 1 => false,
            ComputeOp::Project { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::DedupeBy { .. }
            | ComputeOp::With { .. }
            | ComputeOp::Limit { .. } => ultimate_surface_is_bare_list_producer(state, source),
        },
        DagNodeSource::RelationTraversal { source_label, .. } => {
            ultimate_surface_is_bare_list_producer(state, source_label)
        }
        _ => false,
    }
}

fn predicate_is_absent_or_true(pred: Option<&Predicate>) -> bool {
    matches!(pred, None | Some(Predicate::True))
}
