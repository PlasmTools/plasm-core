//! Compile-time gate: only StaticSingleton field extracts may fill scalar invoke params.

use super::binding_contract::binding_contract;
use super::prelude::*;
use super::schema_validate::cgs_for_qualified_entity;
use super::types::CompileState;
use plasm_core::{plp, FieldType, PlasmInputRef, Value};

/// Reject plural / bounded / whole-entity refs into scalar stringish invoke params (PLP-1).
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
        if !param_is_scalar_cell(&nv.field_type) {
            continue;
        }
        reject_non_static_singleton_scalar_refs(state, node_id, param, val)?;
    }
    if let Some(path_vars) = &inv.path_vars {
        for (param, val) in path_vars {
            let Some(field) = fields.iter().find(|f| f.name == *param) else {
                continue;
            };
            let Ok(nv) = field.named_value(cgs.as_ref()) else {
                continue;
            };
            if !param_is_scalar_cell(&nv.field_type) {
                continue;
            }
            reject_non_static_singleton_scalar_refs(state, node_id, param, val)?;
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

/// Scalar cell params (PLP-1); not arrays / JSON / entity-ref identity slots.
fn param_is_scalar_cell(ft: &FieldType) -> bool {
    matches!(
        ft,
        FieldType::Boolean
            | FieldType::Number
            | FieldType::Integer
            | FieldType::Uuid
            | FieldType::Blob
            | FieldType::String
            | FieldType::Select
            | FieldType::Date
            | FieldType::Money
    )
}

fn reject_non_static_singleton_scalar_refs(
    state: &CompileState<'_>,
    node_id: &str,
    param: &str,
    value: &Value,
) -> Result<(), String> {
    match value {
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) if path.is_empty() => {
            Err(plp::plp4_program(
                node_id,
                format!(
                    "param `{param}` expects a scalar cell, but `{node}` is a whole-entity row — bind `{node}.wire` from a StaticSingleton (Get / nullary singleton), or pass a string literal"
                ),
            ))
        }
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) if !path.is_empty() => {
            if !binding_is_static_singleton(state, node) {
                return Err(plp::plp4_program(
                    node_id,
                    format!(
                        "param `{param}` expects a scalar, but `{node}.{}` is not a StaticSingleton field extract — use a Get / nullary singleton row (`e#(id=…)`) then `ℓ.wire`, not plural / `| take 1` / filtered query field dots",
                        path.join(".")
                    ),
                ));
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_non_static_singleton_scalar_refs(state, node_id, param, item)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for v in fields.values() {
                reject_non_static_singleton_scalar_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        Value::UnionCtor { ctor_fields, .. } => {
            for v in ctor_fields.values() {
                reject_non_static_singleton_scalar_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn binding_is_static_singleton(state: &CompileState<'_>, label: &str) -> bool {
    binding_contract(state, label)
        .is_some_and(|c| matches!(c.row_cardinality, RowCardinalityProof::StaticSingleton))
}
