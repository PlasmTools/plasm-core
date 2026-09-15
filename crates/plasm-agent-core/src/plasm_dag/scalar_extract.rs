//! PLP-1 singleton field-dot → scalar cell extract (shared DAG node + lowerers).

use super::plan_serialize::{collect_template_uses_from_expr, infer_surface_contract};
use super::schema_validate::validate_surface_inline_projection;
use super::types::{CompileState, DagNode, DagNodeSource};
use crate::execute_session::ExecuteSession;
use crate::plasm_plan::PlanNodeKind;
use crate::program_binding::RowCardinalityProof;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::plp;
use plasm_core::Expr;

/// DAG node for a singleton field cell (`ℓ.wire`), checked for exactly one row at runtime.
pub(in crate::plasm_dag) fn scalar_extract_node(
    id: &str,
    expr: &str,
    source: &str,
    wire: String,
) -> DagNode {
    DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: true,
        page_size: None,
        source: DagNodeSource::ScalarExtract {
            source: source.to_string(),
            wire,
        },
    }
}

pub(in crate::plasm_dag) fn reject_non_singleton_field_dot(
    id: &str,
    expr_hint: &str,
    wire: &str,
) -> String {
    plp::plp4_program(
        id,
        format!(
            "`{expr_hint}` field-dot requires a singleton — narrow with `| take 1` before extracting, or use `| select {wire}` for row projection"
        ),
    )
}

/// Binding continuation: `label.wire` for static or bounded singletons.
pub(in crate::plasm_dag) fn lower_binding_scalar_field_dot(
    id: &str,
    expr: &str,
    label: &str,
    wire: String,
    row_cardinality: RowCardinalityProof,
) -> Result<DagNode, String> {
    if !row_cardinality.permits_scalar_field_extract() {
        return Err(reject_non_singleton_field_dot(
            id,
            &format!("{label}.{wire}"),
            &wire,
        ));
    }
    Ok(scalar_extract_node(id, expr, label, wire))
}

/// Catalog surface: `e#.wire` / Get-singleton `.wire` → Get (or singleton surface) + ScalarExtract.
pub(in crate::plasm_dag) fn compile_catalog_singleton_field_dot(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    parsed: ParsedExpr,
    wire: String,
) -> Result<Vec<DagNode>, String> {
    let uses = collect_template_uses_from_expr(&parsed.expr, None);
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    let singleton = matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_));
    if !singleton {
        return Err(reject_non_singleton_field_dot(id, expr, &wire));
    }
    let src_id = format!("__plasm_{id}_extract_src");
    let get_node = DagNode {
        id: src_id.clone(),
        expr: expr.to_string(),
        singleton: true,
        page_size: None,
        source: DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result: uses,
        },
    };
    validate_surface_inline_projection(session, state, &get_node)?;
    let extract = scalar_extract_node(id, expr, &src_id, wire);
    Ok(vec![get_node, extract])
}

/// Lower `e#("id").wire` values inside invoke/create args into Get + ScalarExtract nodes.
pub(in crate::plasm_dag) fn expand_get_scalar_extracts_in_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    expr: &mut Expr,
) -> Result<Vec<DagNode>, String> {
    let mut extras = Vec::new();
    let mut n = 0u32;
    walk_expr_get_extracts(session, state, node_id, expr, &mut extras, &mut n)?;
    Ok(extras)
}

fn walk_expr_get_extracts(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    expr: &mut Expr,
    extras: &mut Vec<DagNode>,
    n: &mut u32,
) -> Result<(), String> {
    match expr {
        Expr::Invoke(inv) => {
            if let Some(input) = inv.input.as_mut() {
                walk_payload_get_extracts(session, state, node_id, input, extras, n)?;
            }
        }
        Expr::Create(c) => {
            walk_payload_get_extracts(session, state, node_id, &mut c.input, extras, n)?;
        }
        Expr::Delete(d) => {
            if let Some(input) = d.input.as_mut() {
                walk_payload_get_extracts(session, state, node_id, input, extras, n)?;
            }
        }
        Expr::Chain(chain) => {
            walk_expr_get_extracts(session, state, node_id, &mut chain.source, extras, n)?;
            if let plasm_core::ChainStep::Explicit { expr } = &mut chain.step {
                walk_expr_get_extracts(session, state, node_id, expr, extras, n)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn walk_payload_get_extracts(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    payload: &mut plasm_core::InvokeInputPayload,
    extras: &mut Vec<DagNode>,
    n: &mut u32,
) -> Result<(), String> {
    match payload {
        plasm_core::InvokeInputPayload::Raw(v) => {
            walk_value_get_extracts(session, state, node_id, v, extras, n)
        }
        plasm_core::InvokeInputPayload::Typed(t) => {
            let mut v = t.to_value();
            walk_value_get_extracts(session, state, node_id, &mut v, extras, n)?;
            *payload = plasm_core::InvokeInputPayload::Raw(v);
            Ok(())
        }
    }
}

fn walk_value_get_extracts(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    value: &mut plasm_core::Value,
    extras: &mut Vec<DagNode>,
    n: &mut u32,
) -> Result<(), String> {
    match value {
        plasm_core::Value::GetScalarExtract(extract) => {
            let extract = extract.clone();
            *n += 1;
            let bind_id = format!("__plasm_{node_id}_gse_{n}");
            let get_expr = get_expr_from_extract(&extract)?;
            let parsed = plasm_core::expr_parser::ParsedExpr::from_expr(get_expr);
            let nodes = compile_catalog_singleton_field_dot(
                session,
                state,
                &bind_id,
                &format!(
                    "{}({:?}).{}",
                    extract.entity, extract.identity, extract.wire
                ),
                parsed,
                extract.wire.clone(),
            )?;
            extras.extend(nodes);
            *value = plasm_core::Value::PlasmInputRef(plasm_core::PlasmInputRef::node_output(
                bind_id,
                Vec::new(),
            ));
            Ok(())
        }
        plasm_core::Value::Array(items) => {
            for item in items {
                walk_value_get_extracts(session, state, node_id, item, extras, n)?;
            }
            Ok(())
        }
        plasm_core::Value::Object(fields)
        | plasm_core::Value::UnionCtor {
            ctor_fields: fields,
            ..
        } => {
            for v in fields.values_mut() {
                walk_value_get_extracts(session, state, node_id, v, extras, n)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn get_expr_from_extract(extract: &plasm_core::GetScalarExtract) -> Result<Expr, String> {
    use plasm_core::{GetExpr, Ref};
    let mut get = match extract.identity.as_ref() {
        plasm_core::Value::String(s) | plasm_core::Value::PhraseIdent(s) => {
            GetExpr::new(extract.entity.as_str(), s.as_str())
        }
        plasm_core::Value::Integer(i) => GetExpr::new(extract.entity.as_str(), i.to_string()),
        plasm_core::Value::PlasmInputRef(r) => {
            GetExpr::from_ref(Ref::simple_binding(extract.entity.as_str(), r.clone()))
        }
        other => {
            return Err(format!(
                "PLP-1: `{}.{}` Get identity must be a literal or binding, got {}",
                extract.entity,
                extract.wire,
                other.type_name()
            ))
        }
    };
    if let Some(entry) = extract.catalog_entry_id.as_deref() {
        get.catalog_entry_id =
            plasm_core::catalog_id::CatalogEntryStamp::some(entry.to_string().into());
    }
    Ok(Expr::Get(get))
}
