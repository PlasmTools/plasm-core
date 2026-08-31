//! Fold hashed `ComputeOp` constructors into a fused [`RowPlan`].

use crate::plasm_monad::{ComputeOp, StepId};

use super::collect::{CollectCardinality, CollectReason, RenderCollectSpec};
use super::error::{FrameSchemaError, FusionError, RowComputeError};
use super::expr::ProjectSpec;
use super::filter::RowFilter;
use super::ids::{FrameId, RowNodeId, SurfaceMeaningId};
use super::plan::{Pipeline, PlanNode, RowPlan, TypedAggregate};
use std::num::NonZeroUsize;

/// Fold a linear Map-spine `ComputeOp` chain. `Render` is a collect barrier, not a node.
pub fn fold_compute_ops(
    ops: &[ComputeOp],
    source: FrameId,
    step: StepId,
    cardinality: CollectCardinality,
) -> Result<RowPlan, RowComputeError> {
    let meaning = SurfaceMeaningId::from_bytes(
        &serde_json::to_vec(ops).unwrap_or_else(|_| ops.len().to_le_bytes().to_vec()),
    );
    let mut pipeline = Pipeline::new();
    let mut collect = CollectReason::ProgramReturn { step: step.clone() };
    for (i, op) in ops.iter().enumerate() {
        let id = RowNodeId::new(i as u64 + 1);
        match op {
            ComputeOp::Render {
                columns,
                template,
                column_aliases,
                render_bindings,
            } => {
                if i + 1 != ops.len() {
                    return Err(FusionError::RenderInPipeline.into());
                }
                collect = CollectReason::Render {
                    step,
                    spec: RenderCollectSpec {
                        columns: columns.clone(),
                        column_aliases: column_aliases.clone(),
                        template: template.clone(),
                        collection_alias: None,
                        render_bindings: render_bindings.clone(),
                    },
                };
                break;
            }
            other => pipeline.push(id, plan_node_from_compute(other)?)?,
        }
    }
    Ok(RowPlan::new(
        source,
        pipeline,
        collect,
        cardinality,
        meaning,
    )?)
}

pub fn plan_node_from_compute(op: &ComputeOp) -> Result<PlanNode, RowComputeError> {
    match op {
        ComputeOp::Filter { predicates } => {
            let filter = RowFilter::new(predicates.clone())?;
            Ok(PlanNode::Filter(filter))
        }
        ComputeOp::Sort { key, descending } => Ok(PlanNode::Sort {
            key: key.clone(),
            descending: *descending,
        }),
        ComputeOp::Limit { count } => {
            let count = NonZeroUsize::new(*count).ok_or(FrameSchemaError::ZeroLimit)?;
            Ok(PlanNode::Limit { count })
        }
        ComputeOp::DedupeBy { keys } => Ok(PlanNode::Dedupe { keys: keys.clone() }),
        ComputeOp::Project { fields } => Ok(PlanNode::Project(ProjectSpec {
            fields: fields.clone(),
        })),
        ComputeOp::With { columns } => {
            if columns.is_empty() {
                return Err(FrameSchemaError::EmptyWith.into());
            }
            Ok(PlanNode::With {
                columns: columns.clone(),
            })
        }
        ComputeOp::GroupBy { keys, aggregates } => {
            if keys.is_empty() {
                return Err(FrameSchemaError::EmptyGroupKeys.into());
            }
            let aggs = aggregates
                .iter()
                .map(TypedAggregate::from_spec)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PlanNode::GroupBy {
                keys: keys.clone(),
                aggs,
            })
        }
        ComputeOp::Aggregate { aggregates } => {
            let aggs = aggregates
                .iter()
                .map(TypedAggregate::from_spec)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PlanNode::Aggregate { aggs })
        }
        ComputeOp::Render { .. } => Err(FusionError::RenderInPipeline.into()),
    }
}
