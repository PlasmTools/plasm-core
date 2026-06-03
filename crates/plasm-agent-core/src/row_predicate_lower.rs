//! Lower [`plasm_core::RowPredicate`] to executable [`PlanPredicate`] IR.

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{PlanPredicate, PlanPredicateOp, PlanValue, QualifiedEntityKey};
use plasm_core::SymbolMapCrossRequestCache;
use plasm_core::{CompOp, RowPredicate, TypedComparisonValue};

pub(crate) fn lower_row_predicate_to_plan(
    pred: &RowPredicate,
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
) -> Result<Vec<PlanPredicate>, String> {
    pred.0
        .iter()
        .map(|c| {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                cross_cache,
                Some(qe),
                c.field.as_str(),
            );
            Ok(PlanPredicate {
                field_path: wire.split('.').map(str::to_string).collect(),
                op: comp_op_to_plan(c.op),
                value: typed_comparison_to_plan_value(&c.value)?,
            })
        })
        .collect()
}

fn comp_op_to_plan(op: CompOp) -> PlanPredicateOp {
    match op {
        CompOp::Eq => PlanPredicateOp::Eq,
        CompOp::Neq => PlanPredicateOp::Ne,
        CompOp::Gt => PlanPredicateOp::Gt,
        CompOp::Lt => PlanPredicateOp::Lt,
        CompOp::Gte => PlanPredicateOp::Gte,
        CompOp::Lte => PlanPredicateOp::Lte,
        CompOp::Contains => PlanPredicateOp::Contains,
        CompOp::In => PlanPredicateOp::In,
        CompOp::Exists => PlanPredicateOp::Exists,
    }
}

fn typed_comparison_to_plan_value(v: &TypedComparisonValue) -> Result<PlanValue, String> {
    let json = serde_json::to_value(v.to_value()).map_err(|e| format!("row filter value: {e}"))?;
    Ok(PlanValue::Literal { value: json })
}
