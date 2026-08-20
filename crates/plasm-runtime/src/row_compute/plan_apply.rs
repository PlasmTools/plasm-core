//! Row-plan application over an ingested frame.

use super::aggregates::{finalize_money_sums, group_by_lf};
use super::json_frame::{col_expr, ColKind, FrameState, IDX_COL};
use super::predicates::pred_expr;
use super::with_expr::{infer_with_kind, with_expr};
use chrono::{DateTime, Utc};
use plasm_core::plasm_monad::WithExpr;
use plasm_core::{PlanNode, RowPlan, TypedAggregate};
use polars::prelude::*;

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
