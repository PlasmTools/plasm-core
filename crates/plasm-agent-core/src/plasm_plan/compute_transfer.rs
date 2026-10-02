//! Compute laws shared by DAG bindings, plan emission, and both plan-state analyses.

use super::{ComputeOp, ResultShape};
use crate::program_binding::{BoundedSingletonKind, RowCardinalityProof};

enum CardinalityRule {
    Singleton,
    Preserve,
    LimitOne,
    Plural,
}

fn cardinality_rule(op: &ComputeOp) -> CardinalityRule {
    match op {
        ComputeOp::MergeBranches { .. }
        | ComputeOp::Aggregate { .. }
        | ComputeOp::Python { per_row: false, .. } => CardinalityRule::Singleton,
        ComputeOp::Render { .. }
        | ComputeOp::Python { per_row: true, .. }
        | ComputeOp::Project { .. }
        | ComputeOp::Filter { .. }
        | ComputeOp::Sort { .. }
        | ComputeOp::DedupeBy { .. }
        | ComputeOp::With { .. } => CardinalityRule::Preserve,
        ComputeOp::Limit { count } if *count <= 1 => CardinalityRule::LimitOne,
        ComputeOp::Limit { .. } | ComputeOp::GroupBy { .. } | ComputeOp::Union { .. } => {
            CardinalityRule::Plural
        }
    }
}

/// The source is lazy because aggregate and other source-independent rules
/// must not recursively classify malformed or cyclic source edges.
pub(crate) fn compute_cardinality_transfer(
    op: &ComputeOp,
    source: impl FnOnce() -> RowCardinalityProof,
) -> RowCardinalityProof {
    match cardinality_rule(op) {
        CardinalityRule::Singleton => RowCardinalityProof::StaticSingleton,
        CardinalityRule::Preserve => source(),
        CardinalityRule::LimitOne => RowCardinalityProof::BoundedSingleton {
            kind: BoundedSingletonKind::LimitOne,
            from_plural_source: matches!(
                source(),
                RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked
            ),
        },
        CardinalityRule::Plural => RowCardinalityProof::StaticPlural,
    }
}

/// Materialized result shape is independent of a proof that a list contains
/// exactly one row. Only explicit render singleton selection changes its shape.
pub(crate) fn compute_result_shape(op: &ComputeOp, explicit_singleton: bool) -> ResultShape {
    match op {
        ComputeOp::Python { per_row: false, .. } | ComputeOp::MergeBranches { .. } => {
            ResultShape::Single
        }
        ComputeOp::Render { .. } if explicit_singleton => ResultShape::Single,
        ComputeOp::Render { .. }
        | ComputeOp::Python { per_row: true, .. }
        | ComputeOp::Project { .. }
        | ComputeOp::Filter { .. }
        | ComputeOp::Sort { .. }
        | ComputeOp::DedupeBy { .. }
        | ComputeOp::With { .. }
        | ComputeOp::Limit { .. }
        | ComputeOp::GroupBy { .. }
        | ComputeOp::Union { .. }
        | ComputeOp::Aggregate { .. } => ResultShape::List,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_count_and_materialized_shape_are_independent() {
        let render = ComputeOp::Render {
            columns: vec![],
            template: String::new(),
            column_aliases: Default::default(),
            render_bindings: vec![],
        };
        assert_eq!(
            compute_cardinality_transfer(&render, || RowCardinalityProof::StaticSingleton),
            RowCardinalityProof::StaticSingleton
        );
        assert_eq!(compute_result_shape(&render, false), ResultShape::List);
        assert_eq!(compute_result_shape(&render, true), ResultShape::Single);

        let aggregate = ComputeOp::Aggregate { aggregates: vec![] };
        assert_eq!(
            compute_cardinality_transfer(&aggregate, || RowCardinalityProof::StaticPlural),
            RowCardinalityProof::StaticSingleton
        );
        assert_eq!(compute_result_shape(&aggregate, false), ResultShape::List);
    }
}
