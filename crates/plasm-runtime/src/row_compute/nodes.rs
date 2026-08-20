//! Apply fused row-plan nodes to a Polars lazy frame.

use super::expressions::{infer_with_kind, pred_expr, with_expr};
use super::json_frame::{col_expr, ColKind, FrameState, IDX_COL};
use super::money::push_money_sum;
use chrono::{DateTime, Utc};
use plasm_core::row_plan::NumericAgg;
use plasm_core::{FieldPath, PlanNode, TypedAggregate};
use polars::prelude::*;

pub(super) fn apply_node(
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
    let mut visible = keys.iter().map(FieldPath::dotted).collect::<Vec<_>>();
    for agg in aggs {
        match agg {
            TypedAggregate::Count { name } => {
                agg_exprs.push(len().alias(name.as_str()));
                visible.push(name.as_str().to_string());
                state.kinds.insert(name.as_str().to_string(), ColKind::Int);
            }
            TypedAggregate::Numeric { name, fn_, field } => {
                if *fn_ == NumericAgg::Sum
                    && state.kinds.get(&field.dotted()) == Some(&ColKind::Money)
                {
                    push_money_sum(&mut agg_exprs, &mut visible, state, name.as_str(), field);
                } else {
                    let c = col_expr(field).cast(DataType::Float64);
                    let e = match fn_ {
                        NumericAgg::Sum => c.sum(),
                        NumericAgg::Avg => c.mean(),
                        NumericAgg::Min => c.min(),
                        NumericAgg::Max => c.max(),
                        NumericAgg::First => c.first(),
                        NumericAgg::Last => c.last(),
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
