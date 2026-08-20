//! Predicate lowering for row-compute plans.

use super::json_frame::col_expr;
use plasm_core::plasm_monad::{PlanPredicate, PlanPredicateOp, PlasmDataValue};
use polars::prelude::*;

pub(super) fn pred_expr(p: &PlanPredicate) -> PolarsResult<Expr> {
    let lhs = col_expr(&p.field_path);
    let rhs = data_lit(&p.value)?;
    Ok(cmp_exprs(p.op, lhs, rhs))
}

pub(super) fn cmp_exprs(op: PlanPredicateOp, l: Expr, r: Expr) -> Expr {
    match op {
        PlanPredicateOp::Eq => l.eq(r),
        PlanPredicateOp::Ne => l.neq(r),
        PlanPredicateOp::Lt => l.lt(r),
        PlanPredicateOp::Lte => l.lt_eq(r),
        PlanPredicateOp::Gt => l.gt(r),
        PlanPredicateOp::Gte => l.gt_eq(r),
        PlanPredicateOp::Contains => l.cast(DataType::String).str().contains(r, false),
        PlanPredicateOp::In => l.is_in(r),
        PlanPredicateOp::Exists => l.is_not_null(),
    }
}

fn data_lit(v: &PlasmDataValue) -> PolarsResult<Expr> {
    match v {
        PlasmDataValue::Literal { value } => json_lit(value),
        PlasmDataValue::Array { items } => {
            let lits: Result<Vec<_>, _> = items.iter().map(data_lit).collect();
            Ok(concat_list(lits?)?)
        }
        other => Err(PolarsError::ComputeError(
            format!("unsupported row-filter value {other:?}").into(),
        )),
    }
}

fn json_lit(v: &serde_json::Value) -> PolarsResult<Expr> {
    Ok(match v {
        serde_json::Value::Null => lit(NULL),
        serde_json::Value::Bool(b) => lit(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                lit(i)
            } else if let Some(f) = n.as_f64() {
                lit(f)
            } else {
                lit(n.to_string())
            }
        }
        serde_json::Value::String(s) => lit(s.as_str()),
        serde_json::Value::Array(items) => {
            let lits: Result<Vec<_>, _> = items.iter().map(json_lit).collect();
            concat_list(lits?)?
        }
        serde_json::Value::Object(_) => lit(v.to_string()),
    })
}
