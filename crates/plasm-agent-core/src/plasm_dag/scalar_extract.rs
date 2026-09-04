//! PLP-1 StaticSingleton field-dot → scalar cell extract (shared DAG node + lowerers).

use super::plan_serialize::{collect_template_uses_from_expr, infer_surface_contract};
use super::schema_validate::validate_surface_inline_projection;
use super::types::{CompileState, DagNode, DagNodeSource};
use crate::execute_session::ExecuteSession;
use crate::plasm_plan::PlanNodeKind;
use crate::program_binding::RowCardinalityProof;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::plp;
use plasm_core::Expr;

/// DAG node for a proven StaticSingleton field cell (`ℓ.wire`).
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

pub(in crate::plasm_dag) fn reject_non_static_field_dot(
    id: &str,
    expr_hint: &str,
    wire: &str,
) -> String {
    plp::plp4_program(
        id,
        format!(
            "`{expr_hint}` field-dot is not a StaticSingleton extract — use `| select {wire}` or `[{wire}]` for row projection"
        ),
    )
}

/// Binding continuation: `label.wire` when `label` is StaticSingleton.
pub(in crate::plasm_dag) fn lower_binding_scalar_field_dot(
    id: &str,
    expr: &str,
    label: &str,
    wire: String,
    row_cardinality: RowCardinalityProof,
) -> Result<DagNode, String> {
    if !matches!(row_cardinality, RowCardinalityProof::StaticSingleton) {
        return Err(reject_non_static_field_dot(
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
    let uses = collect_template_uses_from_expr(&parsed.expr);
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    let singleton = matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_));
    if !singleton {
        return Err(reject_non_static_field_dot(id, expr, &wire));
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
