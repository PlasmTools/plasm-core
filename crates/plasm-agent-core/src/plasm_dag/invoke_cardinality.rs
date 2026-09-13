//! Compile-time gate: only static/bounded singleton field extracts / scalar-cell bindings may fill
//! scalar invoke params (PLP-1).

use super::binding_contract::{binding_contract, reject_illegal_content_stitch};
use super::prelude::*;
use super::schema_validate::cgs_for_qualified_entity;
use super::types::CompileState;
use plasm_core::{plp, FieldType, PlasmInputRef, Value};

/// Reject plural / unproven / entity-row refs into scalar stringish invoke/create params (PLP-1).
pub(in crate::plasm_dag) fn validate_invoke_scalar_field_refs(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    expr: &Expr,
) -> Result<(), String> {
    let (capability, entity, catalog_entry_id, input) = match expr {
        Expr::Invoke(inv) => {
            let Some(input) = &inv.input else {
                return Ok(());
            };
            (
                inv.capability.as_str(),
                inv.target.entity_type.as_str(),
                inv.catalog_entry_id.as_deref(),
                input,
            )
        }
        Expr::Delete(delete) => {
            let Some(input) = &delete.input else {
                return Ok(());
            };
            (
                delete.capability.as_str(),
                delete.target.entity_type.as_str(),
                delete.catalog_entry_id.as_deref(),
                input,
            )
        }
        Expr::Create(c) => (
            c.capability.as_str(),
            c.entity.as_str(),
            c.catalog_entry_id.as_deref(),
            &c.input,
        ),
        Expr::Chain(chain) => {
            validate_invoke_scalar_field_refs(session, state, node_id, &chain.source)?;
            if let plasm_core::ChainStep::Explicit { expr } = &chain.step {
                validate_invoke_scalar_field_refs(session, state, node_id, expr)?;
            }
            return Ok(());
        }
        Expr::Query(_)
        | Expr::Get(_)
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_)
        | Expr::TeachingValue { .. } => return Ok(()),
    };
    let qe = QualifiedEntityKey {
        entry_id: catalog_entry_id
            .unwrap_or(session.entry_id.as_str())
            .to_string(),
        entity: entity.to_string(),
    };
    let cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let Some(cap) = cgs.get_capability(capability) else {
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
        reject_non_scalar_cell_invoke_refs(state, node_id, param, val)?;
    }
    Ok(())
}

/// Scalar cell params (PLP-1); not arrays / JSON / entity-ref identity slots.
fn param_is_scalar_cell(ft: &FieldType) -> bool {
    matches!(
        ft,
        FieldType::Boolean
            | FieldType::Number
            | FieldType::Integer
            | FieldType::Uuid
            | FieldType::DigitId
            | FieldType::Blob
            | FieldType::String
            | FieldType::Select
            | FieldType::Date
            | FieldType::Money
    )
}

fn reject_non_scalar_cell_invoke_refs(
    state: &CompileState<'_>,
    node_id: &str,
    param: &str,
    value: &Value,
) -> Result<(), String> {
    match value {
        Value::GetScalarExtract(_) => Ok(()),
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) if path.is_empty() => {
            if binding_is_scalar_cell(state, node) {
                Ok(())
            } else {
                Err(plp::plp4_program(
                    node_id,
                    format!(
                        "param `{param}` expects a scalar cell, but `{node}` denotes an entity row — bind a scalar cell (`x = ℓ.wire` or a string/heredoc binding) then pass `param=x`, or write `param=ℓ.wire` inline"
                    ),
                ))
            }
        }
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) if !path.is_empty() => {
            reject_illegal_content_stitch(state, node, path)?;
            if !binding_permits_scalar_field_extract(state, node) {
                return Err(plp::plp4_program(
                    node_id,
                    format!(
                        "param `{param}` expects a scalar, but `{node}.{}` is not a singleton field extract — use a Get or bind `rows | take 1`, then extract the field with `ℓ.wire`",
                        path.join(".")
                    ),
                ));
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_non_scalar_cell_invoke_refs(state, node_id, param, item)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for v in fields.values() {
                reject_non_scalar_cell_invoke_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        Value::UnionCtor { ctor_fields, .. } => {
            for v in ctor_fields.values() {
                reject_non_scalar_cell_invoke_refs(state, node_id, param, v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn binding_permits_scalar_field_extract(state: &CompileState<'_>, label: &str) -> bool {
    binding_contract(state, label).is_some_and(|c| c.row_cardinality.permits_scalar_field_extract())
}

fn binding_is_scalar_cell(state: &CompileState<'_>, label: &str) -> bool {
    binding_contract(state, label).is_some_and(|c| c.is_scalar_cell())
}
