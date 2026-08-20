//! Polars lowering for `.with(...)` row-compute expressions.

use super::json_frame::{col_expr, ColKind, FrameState, MONEY_AMOUNT, MONEY_CCY};
use super::predicates::cmp_exprs;
use chrono::{DateTime, Utc};
use plasm_core::plasm_monad::{FieldPath, WithExpr, WithLiteral};
use plasm_core::{normalize_temporal_value, ArithOp, TemporalWireFormat};
use polars::prelude::*;

pub(super) fn with_expr(
    expr: &WithExpr,
    state: &FrameState,
    now: DateTime<Utc>,
) -> PolarsResult<Expr> {
    match expr {
        WithExpr::Field(path) => Ok(col_expr(path)),
        WithExpr::Now => Ok(lit(now.to_rfc3339())),
        WithExpr::Literal(litv) => Ok(match litv {
            WithLiteral::Null => lit(NULL),
            WithLiteral::Bool(b) => lit(*b),
            WithLiteral::Integer(i) => lit(*i),
            WithLiteral::Number(s) => {
                if let Ok(i) = s.parse::<i64>() {
                    lit(i)
                } else if let Ok(f) = s.parse::<f64>() {
                    lit(f)
                } else {
                    lit(s.as_str())
                }
            }
            WithLiteral::String(s) => lit(s.as_str()),
        }),
        WithExpr::Arith { op, lhs, rhs }
            if *op == ArithOp::Sub && is_temporal_sub(lhs, rhs, state) =>
        {
            temporal_sub_days(now, lhs, rhs)
        }
        WithExpr::Arith { op, lhs, rhs } => {
            let l_kind = infer_with_kind(lhs, state);
            let r_kind = infer_with_kind(rhs, state);
            let l = with_expr(lhs, state, now)?;
            let r = with_expr(rhs, state, now)?;
            arith_expr(*op, l, r, l_kind, r_kind)
        }
        WithExpr::Len { field } => Ok(col_expr(field)
            .cast(DataType::String)
            .str()
            .len_chars()
            .cast(DataType::Int64)),
        WithExpr::When {
            lhs,
            op,
            rhs,
            then,
            else_,
        } => {
            let l = with_expr(lhs, state, now)?;
            let r = with_expr(rhs, state, now)?;
            Ok(when(cmp_exprs(*op, l, r))
                .then(with_expr(then, state, now)?)
                .otherwise(with_expr(else_, state, now)?))
        }
    }
}

fn is_now(expr: &WithExpr) -> bool {
    matches!(expr, WithExpr::Now)
}

fn is_temporal_operand(expr: &WithExpr, state: &FrameState) -> bool {
    match expr {
        WithExpr::Now => true,
        WithExpr::Literal(WithLiteral::String(_)) => true,
        WithExpr::Field(p) => matches!(
            state.kinds.get(&p.dotted()),
            Some(ColKind::Str | ColKind::Temporal)
        ),
        _ => false,
    }
}

fn is_temporal_sub(lhs: &WithExpr, rhs: &WithExpr, state: &FrameState) -> bool {
    is_now(lhs)
        || is_now(rhs)
        || (is_temporal_operand(lhs, state) && is_temporal_operand(rhs, state))
}

fn utc_from_raw(raw: &str) -> Option<DateTime<Utc>> {
    normalize_temporal_value(
        plasm_core::Value::String(raw.to_string()),
        TemporalWireFormat::Rfc3339,
    )
    .ok()
    .and_then(|v| match v {
        plasm_core::Value::String(iso) => DateTime::parse_from_rfc3339(&iso)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        _ => None,
    })
}

const MS_PER_DAY: i64 = 86_400_000;

fn col_to_epoch_millis(field: &FieldPath) -> Expr {
    col_expr(field).map(
        move |s| {
            let out: Vec<Option<i64>> = match s.dtype() {
                DataType::String => s
                    .str()
                    .map(|ca| {
                        ca.into_iter()
                            .map(|opt| {
                                opt.and_then(|raw| {
                                    utc_from_raw(raw).map(|dt| dt.timestamp_millis())
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => vec![None; s.len()],
            };
            Ok(Some(Column::new(s.name().clone(), out)))
        },
        GetOutput::from_type(DataType::Int64),
    )
}

fn temporal_millis_expr(expr: &WithExpr, now: DateTime<Utc>) -> PolarsResult<Expr> {
    match expr {
        WithExpr::Now => Ok(lit(now.timestamp_millis())),
        WithExpr::Field(field) => Ok(col_to_epoch_millis(field)),
        WithExpr::Literal(WithLiteral::String(s)) => Ok(match utc_from_raw(s) {
            Some(dt) => lit(dt.timestamp_millis()),
            None => lit(NULL).cast(DataType::Int64),
        }),
        _ => Err(PolarsError::ComputeError(
            "temporal subtraction requires temporal fields or `now`".into(),
        )),
    }
}

fn temporal_sub_days(now: DateTime<Utc>, lhs: &WithExpr, rhs: &WithExpr) -> PolarsResult<Expr> {
    let l = temporal_millis_expr(lhs, now)?;
    let r = temporal_millis_expr(rhs, now)?;
    Ok((l - r) / lit(MS_PER_DAY))
}

fn arith_expr(
    op: ArithOp,
    l: Expr,
    r: Expr,
    l_kind: ColKind,
    r_kind: ColKind,
) -> PolarsResult<Expr> {
    let money_l = l_kind == ColKind::Money;
    let money_r = r_kind == ColKind::Money;
    if !money_l && !money_r {
        let string_add = op == ArithOp::Add
            && (l_kind == ColKind::Str || r_kind == ColKind::Str)
            && l_kind != ColKind::Temporal
            && r_kind != ColKind::Temporal;
        if string_add {
            return Ok(l.cast(DataType::String) + r.cast(DataType::String));
        }
        let coerce = op == ArithOp::Div
            || matches!(l_kind, ColKind::Str | ColKind::Json | ColKind::Float)
            || matches!(r_kind, ColKind::Str | ColKind::Json | ColKind::Float);
        let l = if coerce { l.cast(DataType::Float64) } else { l };
        let r = if coerce { r.cast(DataType::Float64) } else { r };
        return Ok(match op {
            ArithOp::Add => l + r,
            ArithOp::Sub => l - r,
            ArithOp::Mul => l * r,
            ArithOp::Div => l / r,
        });
    }
    let l_amt = if money_l {
        l.clone()
            .struct_()
            .field_by_name(MONEY_AMOUNT)
            .cast(DataType::Decimal(Some(38), Some(8)))
    } else {
        l.clone().cast(DataType::Decimal(Some(38), Some(8)))
    };
    let r_amt = if money_r {
        r.clone()
            .struct_()
            .field_by_name(MONEY_AMOUNT)
            .cast(DataType::Decimal(Some(38), Some(8)))
    } else {
        r.clone().cast(DataType::Decimal(Some(38), Some(8)))
    };
    let amount = match op {
        ArithOp::Add => l_amt + r_amt,
        ArithOp::Sub => l_amt - r_amt,
        ArithOp::Mul => l_amt * r_amt,
        ArithOp::Div => l_amt / r_amt,
    };
    let ccy = if money_l {
        l.struct_().field_by_name(MONEY_CCY)
    } else {
        r.struct_().field_by_name(MONEY_CCY)
    };
    Ok(as_struct(vec![
        amount.cast(DataType::String).alias(MONEY_AMOUNT),
        ccy.alias(MONEY_CCY),
    ]))
}

pub(super) fn infer_with_kind(expr: &WithExpr, state: &FrameState) -> ColKind {
    match expr {
        WithExpr::Field(p) => state
            .kinds
            .get(&p.dotted())
            .copied()
            .unwrap_or(ColKind::Json),
        WithExpr::Now => ColKind::Temporal,
        WithExpr::Literal(WithLiteral::Bool(_)) => ColKind::Bool,
        WithExpr::Literal(WithLiteral::Integer(_)) => ColKind::Int,
        WithExpr::Literal(WithLiteral::Number(_)) => ColKind::Float,
        WithExpr::Literal(WithLiteral::String(_)) => ColKind::Str,
        WithExpr::Literal(WithLiteral::Null) => ColKind::Json,
        WithExpr::Len { .. } => ColKind::Int,
        WithExpr::Arith { op, lhs, rhs } => {
            if *op == ArithOp::Sub && is_temporal_sub(lhs, rhs, state) {
                return ColKind::Int;
            }
            let l = infer_with_kind(lhs, state);
            let r = infer_with_kind(rhs, state);
            if *op == ArithOp::Add
                && (l == ColKind::Str || r == ColKind::Str)
                && l != ColKind::Temporal
                && r != ColKind::Temporal
                && l != ColKind::Money
                && r != ColKind::Money
            {
                return ColKind::Str;
            }
            if l == ColKind::Money || r == ColKind::Money {
                ColKind::Money
            } else if *op == ArithOp::Div || l == ColKind::Float || r == ColKind::Float {
                ColKind::Float
            } else {
                l
            }
        }
        WithExpr::When { then, .. } => infer_with_kind(then, state),
    }
}
