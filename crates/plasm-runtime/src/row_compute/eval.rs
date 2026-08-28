//! Apply a fused [`RowPlan`] on an ingested frame.

use super::json_frame::{
    col_expr, collect_json, ingest_json_rows, ColKind, FrameState, IDX_COL, MONEY_AMOUNT, MONEY_CCY,
};
use chrono::{DateTime, Utc};
use plasm_core::plasm_monad::{
    ComputeOp, FieldPath, PlanPredicate, PlanPredicateOp, PlasmDataValue, WithExpr, WithLiteral,
};
use plasm_core::{
    fold_compute_ops, normalize_temporal_value, ArithOp, CollectCardinality, CollectReason,
    FrameId, PlanNode, RowPlan, StepId, TemporalWireFormat, TypedAggregate,
};
use polars::prelude::*;
use rust_decimal::Decimal;

/// Engine collect before host Minijinja (Render is not a PlanNode).
#[derive(Debug, Clone)]
pub enum ComputeEvalOutcome {
    Rows(Vec<serde_json::Value>),
    Render {
        rows: Vec<serde_json::Value>,
        columns: Vec<plasm_core::OutputName>,
        column_aliases: std::collections::BTreeMap<String, plasm_core::OutputName>,
        template: String,
        collection_alias: Option<plasm_core::OutputName>,
        render_bindings: Vec<plasm_core::OutputName>,
    },
}

pub fn eval_compute_ops(
    ops: &[ComputeOp],
    rows: &[serde_json::Value],
) -> Result<ComputeEvalOutcome, String> {
    let step = StepId::new("row").map_err(|e| e.to_string())?;
    let plan = fold_compute_ops(ops, FrameId::new(1), step, CollectCardinality::List)
        .map_err(|e| e.to_string())?;
    let mut state = ingest_json_rows(rows).map_err(|e| e.to_string())?;
    apply_stored_plan(&plan, &mut state).map_err(|e| e.to_string())?;
    let collected = collect_json(&state).map_err(|e| e.to_string())?;
    match plan.collect() {
        CollectReason::Render { spec, .. } => Ok(ComputeEvalOutcome::Render {
            rows: collected,
            columns: spec.columns.clone(),
            column_aliases: spec.column_aliases.clone(),
            template: spec.template.clone(),
            collection_alias: spec.collection_alias.clone(),
            render_bindings: spec.render_bindings.clone(),
        }),
        _ => Ok(ComputeEvalOutcome::Rows(collected)),
    }
}

pub(super) fn apply_stored_plan(plan: &RowPlan, state: &mut FrameState) -> PolarsResult<()> {
    let now = Utc::now();
    ensure_plan_columns(plan, state)?;
    let mut lf = state.df.clone().lazy();
    for (_, node) in plan.nodes().iter() {
        lf = apply_node(lf, node, state, now)?;
    }
    state.df = lf.collect()?;
    finalize_money_sums(state)?;
    Ok(())
}

fn ensure_plan_columns(plan: &RowPlan, state: &mut FrameState) -> PolarsResult<()> {
    let mut names = Vec::new();
    for (_, node) in plan.nodes().iter() {
        collect_node_columns(node, &mut names);
    }
    let height = state.df.height();
    for name in names {
        if state.df.column(&name).is_ok() {
            continue;
        }
        let series = Series::full_null(PlSmallStr::from_str(&name), height, &DataType::Null);
        state.df.with_column(series)?;
        if !state.visible.iter().any(|v| v == &name) {
            state.visible.push(name.clone());
        }
        state.kinds.entry(name).or_insert(ColKind::Json);
    }
    Ok(())
}

fn collect_node_columns(node: &PlanNode, names: &mut Vec<String>) {
    match node {
        PlanNode::Filter(filter) => {
            for p in filter.predicates() {
                names.push(p.field_path.dotted());
            }
        }
        PlanNode::Sort { key, .. } => names.push(key.dotted()),
        PlanNode::GroupBy { keys, aggs } => {
            names.extend(keys.iter().map(|k| k.dotted()));
            for agg in aggs {
                collect_agg_columns(agg, names);
            }
        }
        PlanNode::Aggregate { aggs } => {
            for agg in aggs {
                collect_agg_columns(agg, names);
            }
        }
        PlanNode::Project(spec) => {
            names.extend(spec.fields.values().map(|p| p.dotted()));
        }
        PlanNode::With { columns } => {
            for col in columns {
                collect_with_columns(&col.expr, names);
            }
        }
        PlanNode::Limit { .. } => {}
        PlanNode::Dedupe { keys } | PlanNode::Distinct { keys } => {
            names.extend(keys.iter().map(|k| k.dotted()));
        }
    }
}

fn collect_agg_columns(agg: &TypedAggregate, names: &mut Vec<String>) {
    match agg {
        TypedAggregate::Count { .. } => {}
        TypedAggregate::Numeric { field, .. } | TypedAggregate::MoneySum { field, .. } => {
            names.push(field.dotted());
        }
    }
}

fn collect_with_columns(expr: &WithExpr, names: &mut Vec<String>) {
    match expr {
        WithExpr::Field(p) | WithExpr::Len { field: p } => {
            names.push(p.dotted());
        }
        WithExpr::Literal(_) | WithExpr::Now => {}
        WithExpr::Arith { lhs, rhs, .. } => {
            collect_with_columns(lhs, names);
            collect_with_columns(rhs, names);
        }
        WithExpr::When {
            lhs,
            rhs,
            then,
            else_,
            ..
        } => {
            collect_with_columns(lhs, names);
            collect_with_columns(rhs, names);
            collect_with_columns(then, names);
            collect_with_columns(else_, names);
        }
    }
}

fn finalize_money_sums(state: &mut FrameState) -> PolarsResult<()> {
    let names = std::mem::take(&mut state.money_sum_names);
    for name in names {
        let n_col = format!("__ccy_n_{name}");
        let c_col = format!("__ccy_{name}");
        let n_unique = state.df.column(&n_col)?;
        let ccys = state.df.column(&c_col)?;
        for i in 0..state.df.height() {
            let n = match n_unique.get(i)? {
                AnyValue::UInt32(n) => n as u64,
                AnyValue::UInt64(n) => n,
                AnyValue::Int64(n) if n >= 0 => n as u64,
                AnyValue::Int32(n) if n >= 0 => n as u64,
                AnyValue::Null => 0,
                other => {
                    return Err(PolarsError::ComputeError(
                        format!("unexpected currency-count dtype {other:?}").into(),
                    ))
                }
            };
            if n > 1 {
                let left = match ccys.get(i)? {
                    AnyValue::String(s) => s.to_string(),
                    AnyValue::StringOwned(s) => s.as_str().to_string(),
                    _ => "left".into(),
                };
                return Err(PolarsError::ComputeError(
                    format!("cannot compare money in {left} to money in another currency").into(),
                ));
            }
        }
        let amounts = state.df.column(&name)?;
        let mut encoded: Vec<Option<String>> = Vec::with_capacity(state.df.height());
        for i in 0..state.df.height() {
            let amount = any_amount_string(amounts.get(i)?)?;
            let ccy = match ccys.get(i)? {
                AnyValue::String(s) => s.to_string(),
                AnyValue::StringOwned(s) => s.as_str().to_string(),
                AnyValue::Null => String::new(),
                other => other.to_string(),
            };
            let mut map = serde_json::Map::new();
            map.insert("__plasm_money".into(), serde_json::Value::String(amount));
            if !ccy.is_empty() {
                map.insert("currency".into(), serde_json::Value::String(ccy));
            }
            encoded.push(Some(serde_json::Value::Object(map).to_string()));
        }
        let series = Series::new(PlSmallStr::from_str(&name), encoded);
        state.df.with_column(series)?;
        let _ = state.df.drop_in_place(&n_col);
        let _ = state.df.drop_in_place(&c_col);
        state.kinds.insert(name, ColKind::Json);
    }
    Ok(())
}

fn any_amount_string(v: AnyValue<'_>) -> PolarsResult<String> {
    Ok(match v {
        AnyValue::Decimal(unscaled, scale) => format_decimal_i128(unscaled, scale),
        AnyValue::Float64(f) => trim_float(f),
        AnyValue::Float32(f) => trim_float(f as f64),
        AnyValue::Int64(i) => i.to_string(),
        AnyValue::Int32(i) => i.to_string(),
        AnyValue::String(s) => s.to_string(),
        AnyValue::StringOwned(s) => s.as_str().to_string(),
        AnyValue::Null => "0".into(),
        other => {
            return Err(PolarsError::ComputeError(
                format!("cannot encode money amount from {other:?}").into(),
            ))
        }
    })
}

fn format_decimal_i128(unscaled: i128, scale: usize) -> String {
    Decimal::from_i128_with_scale(unscaled, scale as u32)
        .normalize()
        .to_string()
}

fn trim_float(f: f64) -> String {
    let d = Decimal::from_f64_retain(f)
        .unwrap_or(Decimal::ZERO)
        .normalize();
    d.to_string()
}

fn apply_node(
    lf: LazyFrame,
    node: &PlanNode,
    state: &mut FrameState,
    now: DateTime<Utc>,
) -> PolarsResult<LazyFrame> {
    match node {
        PlanNode::Filter(filter) => {
            let mut e = lit(true);
            for p in filter.predicates() {
                e = e.and(pred_expr(p)?);
            }
            Ok(lf.filter(e))
        }
        PlanNode::Sort { key, descending } => Ok(lf.sort(
            [key.dotted()],
            SortMultipleOptions::default()
                .with_order_descending(*descending)
                .with_nulls_last(true)
                .with_maintain_order(true),
        )),
        PlanNode::Limit { count } => Ok(lf.slice(0, count.get() as u32)),
        PlanNode::Dedupe { keys } | PlanNode::Distinct { keys } => {
            let subset: Option<Vec<PlSmallStr>> = if keys.is_empty() {
                None
            } else {
                Some(
                    keys.iter()
                        .map(|k| PlSmallStr::from_string(k.dotted()))
                        .collect(),
                )
            };
            Ok(lf.unique_stable(subset, UniqueKeepStrategy::First))
        }
        PlanNode::Project(spec) => {
            let mut exprs = vec![col(IDX_COL)];
            let mut visible = Vec::new();
            for (name, path) in &spec.fields {
                exprs.push(col_expr(path).alias(name.as_str()));
                visible.push(name.as_str().to_string());
                if let Some(k) = state.kinds.get(&path.dotted()).copied() {
                    state.kinds.insert(name.as_str().to_string(), k);
                }
            }
            state.visible = visible;
            Ok(lf.select(exprs))
        }
        PlanNode::With { columns } => {
            let mut exprs = Vec::new();
            for col_def in columns {
                let e = with_expr(&col_def.expr, state, now)?;
                let name = col_def.name.as_str();
                state.visible.push(name.to_string());
                state
                    .kinds
                    .insert(name.to_string(), infer_with_kind(&col_def.expr, state));
                exprs.push(e.alias(name));
            }
            Ok(lf.with_columns(exprs))
        }
        PlanNode::GroupBy { keys, aggs } => group_by_lf(lf, keys, aggs, state, true),
        PlanNode::Aggregate { aggs } => group_by_lf(lf, &[], aggs, state, false),
    }
}

fn group_by_lf(
    lf: LazyFrame,
    keys: &[FieldPath],
    aggs: &[TypedAggregate],
    state: &mut FrameState,
    grouped: bool,
) -> PolarsResult<LazyFrame> {
    let mut agg_exprs = Vec::new();
    let mut visible = Vec::new();
    for k in keys {
        visible.push(k.dotted());
    }
    for agg in aggs {
        match agg {
            TypedAggregate::Count { name } => {
                agg_exprs.push(len().alias(name.as_str()));
                visible.push(name.as_str().to_string());
                state.kinds.insert(name.as_str().to_string(), ColKind::Int);
            }
            TypedAggregate::Numeric { name, fn_, field } => {
                if *fn_ == plasm_core::row_plan::NumericAgg::Sum
                    && state.kinds.get(&field.dotted()) == Some(&ColKind::Money)
                {
                    push_money_sum(&mut agg_exprs, &mut visible, state, name.as_str(), field);
                } else {
                    let c = col_expr(field).cast(DataType::Float64);
                    let e = match fn_ {
                        plasm_core::row_plan::NumericAgg::Sum => c.sum(),
                        plasm_core::row_plan::NumericAgg::Avg => c.mean(),
                        plasm_core::row_plan::NumericAgg::Min => c.min(),
                        plasm_core::row_plan::NumericAgg::Max => c.max(),
                        plasm_core::row_plan::NumericAgg::First => c.first(),
                        plasm_core::row_plan::NumericAgg::Last => c.last(),
                    };
                    agg_exprs.push(e.alias(name.as_str()));
                    visible.push(name.as_str().to_string());
                    state
                        .kinds
                        .insert(name.as_str().to_string(), ColKind::Float);
                }
            }
            TypedAggregate::MoneySum { name, field, .. } => {
                push_money_sum(&mut agg_exprs, &mut visible, state, name.as_str(), field);
            }
        }
    }
    state.visible = visible;
    if grouped {
        Ok(lf
            .group_by(keys.iter().map(|k| col(k.dotted())).collect::<Vec<_>>())
            .agg(agg_exprs))
    } else {
        Ok(lf.select(agg_exprs))
    }
}

fn push_money_sum(
    agg_exprs: &mut Vec<Expr>,
    visible: &mut Vec<String>,
    state: &mut FrameState,
    name: &str,
    field: &FieldPath,
) {
    let amount = col_expr(field)
        .struct_()
        .field_by_name(MONEY_AMOUNT)
        .cast(DataType::Decimal(Some(38), Some(8)));
    let ccy = col_expr(field).struct_().field_by_name(MONEY_CCY);
    agg_exprs.push(ccy.clone().n_unique().alias(format!("__ccy_n_{name}")));
    agg_exprs.push(ccy.first().alias(format!("__ccy_{name}")));
    agg_exprs.push(amount.sum().alias(name));
    visible.push(name.to_string());
    state.kinds.insert(name.to_string(), ColKind::Money);
    state.money_sum_names.push(name.to_string());
}

fn pred_expr(p: &PlanPredicate) -> PolarsResult<Expr> {
    let lhs = col_expr(&p.field_path);
    let rhs = data_lit(&p.value)?;
    Ok(match p.op {
        PlanPredicateOp::Eq => lhs.eq(rhs),
        PlanPredicateOp::Ne => lhs.neq(rhs),
        PlanPredicateOp::Lt => lhs.lt(rhs),
        PlanPredicateOp::Lte => lhs.lt_eq(rhs),
        PlanPredicateOp::Gt => lhs.gt(rhs),
        PlanPredicateOp::Gte => lhs.gt_eq(rhs),
        PlanPredicateOp::Contains => lhs.cast(DataType::String).str().contains(rhs, false),
        PlanPredicateOp::In => lhs.is_in(rhs),
        PlanPredicateOp::Exists => lhs.is_not_null(),
    })
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

fn with_expr(expr: &WithExpr, state: &FrameState, now: DateTime<Utc>) -> PolarsResult<Expr> {
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

fn cmp_exprs(op: PlanPredicateOp, l: Expr, r: Expr) -> Expr {
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

fn infer_with_kind(expr: &WithExpr, state: &FrameState) -> ColKind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::parse_with_body;
    use std::str::FromStr;

    #[test]
    fn filter_sort_limit_roundtrip() {
        let rows = vec![
            serde_json::json!({"owner":"alice","score":10}),
            serde_json::json!({"owner":"bob","score":30}),
            serde_json::json!({"owner":"alice","score":20}),
        ];
        let pred = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::Eq,
            value: PlasmDataValue::Literal {
                value: serde_json::json!("alice"),
            },
        };
        let ops = vec![
            ComputeOp::Filter {
                predicates: vec![pred],
            },
            ComputeOp::Sort {
                key: FieldPath::from_dotted("score").unwrap(),
                descending: true,
            },
            ComputeOp::Limit { count: 1 },
        ];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["score"], serde_json::json!(20));
    }

    #[test]
    fn with_mul_adds_column() {
        let rows = vec![serde_json::json!({"quantity": 2, "price": 5})];
        let columns = parse_with_body("notional: quantity * price").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out[0]["notional"], serde_json::json!(10));
        assert_eq!(out[0]["quantity"], serde_json::json!(2));
    }

    #[test]
    fn with_now_minus_field_is_nonnegative_int_days() {
        let rows = vec![
            serde_json::json!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            serde_json::json!({"id": "new", "updated_at": "2024-06-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("age_days: (now - updated_at)").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        let older = out.iter().find(|r| r["id"] == "old").unwrap();
        let newer = out.iter().find(|r| r["id"] == "new").unwrap();
        let age_old = older["age_days"].as_i64().expect("age int");
        let age_new = newer["age_days"].as_i64().expect("age int");
        assert!(age_old >= 0 && age_new >= 0, "ages {age_old} {age_new}");
        assert!(
            age_old > age_new,
            "older row must have larger age: {age_old} vs {age_new}"
        );
    }

    #[test]
    fn with_field_minus_field_is_int_days() {
        let rows = vec![serde_json::json!({
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-11T00:00:00Z",
        })];
        let columns = parse_with_body("cycle: (updated_at - created_at)").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["cycle"], serde_json::json!(10));
    }

    #[test]
    fn with_div_is_float() {
        let rows = vec![serde_json::json!({"quantity": 10, "price": 4})];
        let columns = parse_with_body("rate: quantity / price").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["rate"].as_f64().unwrap(), 2.5);
    }

    #[test]
    fn with_string_plus_concat() {
        let rows = vec![serde_json::json!({"first": "al", "last": "ice"})];
        let columns = parse_with_body("name: first + last").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["name"], serde_json::json!("alice"));
    }

    #[test]
    fn with_when_len_and_temporal_cmp() {
        let rows = vec![
            serde_json::json!({
                "title": "",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-02T00:00:00Z",
            }),
            serde_json::json!({
                "title": "ok",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-20T00:00:00Z",
            }),
        ];
        let columns = parse_with_body(
            "blank: when(len(title)=0, 1, 0), long: when(updated_at - created_at > 5, 1, 0)",
        )
        .unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["blank"], serde_json::json!(1));
        assert_eq!(out[0]["long"], serde_json::json!(0));
        assert_eq!(out[1]["blank"], serde_json::json!(0));
        assert_eq!(out[1]["long"], serde_json::json!(1));
    }

    #[test]
    fn with_when_now_minus_gt() {
        let rows = vec![
            serde_json::json!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            serde_json::json!({"id": "future", "updated_at": "2099-01-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("stale: when(now - updated_at > 14, 1, 0)").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        let old = out.iter().find(|r| r["id"] == "old").unwrap();
        let future = out.iter().find(|r| r["id"] == "future").unwrap();
        assert_eq!(old["stale"], serde_json::json!(1));
        assert_eq!(future["stale"], serde_json::json!(0));
    }

    #[test]
    fn group_by_count() {
        let rows = vec![
            serde_json::json!({"owner":"a","score":1}),
            serde_json::json!({"owner":"a","score":2}),
            serde_json::json!({"owner":"b","score":3}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("owner").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("n").unwrap(),
                function: plasm_core::AggregateFunction::Count,
                field: None,
            }],
        }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn money_sum_same_currency() {
        let rows = vec![
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.50","currency":"USD"}}),
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"2.50","currency":"USD"}}),
            serde_json::json!({"symbol":"B","fee":{"__plasm_money":"4.00","currency":"USD"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 2, "out={out:?}");
        let a = out.iter().find(|r| r["symbol"] == "A").unwrap();
        let got = a["fees"]["__plasm_money"].as_str().expect("money amount");
        assert_eq!(
            Decimal::from_str(got).unwrap(),
            Decimal::from_str("4.00").unwrap(),
            "row={a:?}"
        );
        assert_eq!(a["fees"]["currency"], "USD");
        assert!(a.get("__ccy_n").is_none());
        assert!(a.get("__ccy_n_fees").is_none());
    }

    #[test]
    fn money_sum_rejects_cross_currency() {
        let rows = vec![
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"USD"}}),
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"EUR"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let err = eval_compute_ops(&ops, &rows).unwrap_err();
        assert!(
            err.contains("currency") || err.contains("money"),
            "expected cross-currency error, got {err}"
        );
    }
}
