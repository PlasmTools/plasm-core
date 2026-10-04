//! Compute laws shared by DAG bindings, plan emission, and both plan-state analyses.

use super::{ComputeOp, ResultShape};
use crate::program_binding::{BoundedSingletonKind, RowCardinalityProof};
use plasm_core::plasm_monad::ScopedOutput;

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

/// A correlated record/quantifier emits one row per parent; a filter may drop
/// a parent but cannot multiply it. A rowset bind may emit any number. Use
/// the same at-most-one law at binding and both
/// plan-validation stages so Python input mode cannot diverge downstream.
pub(crate) fn map_body_cardinality_transfer(
    output: &ScopedOutput,
    parent: impl FnOnce() -> RowCardinalityProof,
) -> RowCardinalityProof {
    match output {
        ScopedOutput::Record | ScopedOutput::Filter | ScopedOutput::Quantify { .. } => parent(),
        ScopedOutput::Rows { .. } => RowCardinalityProof::StaticPlural,
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
    use plasm_core::plasm_monad::{PlanQualifiedEntityKey, SyntheticResultSchema};
    use proptest::prelude::*;

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

    proptest! {
        #[test]
        fn correlated_output_cardinality_follows_parent_when_it_cannot_multiply_rows(
            output_kind in 0u8..4,
            parent_kind in 0u8..4,
        ) {
            let output = match output_kind {
                0 => ScopedOutput::Record,
                1 => ScopedOutput::Filter,
                2 => ScopedOutput::Quantify { all: true },
                _ => ScopedOutput::Rows {
                    entity: PlanQualifiedEntityKey {
                        entry_id: "fixture".into(),
                        entity: "Item".into(),
                    },
                    schema: SyntheticResultSchema {
                        optional_fields: Default::default(),
                        entity: None,
                        fields: vec![],
                    },
                    entity_authority: false,
                    acknowledgement: false,
                },
            };
            let parent = match parent_kind {
                0 => RowCardinalityProof::StaticSingleton,
                1 => RowCardinalityProof::StaticPlural,
                2 => RowCardinalityProof::RuntimeChecked,
                _ => RowCardinalityProof::BoundedSingleton {
                    kind: BoundedSingletonKind::LimitOne,
                    from_plural_source: true,
                },
            };
            let actual = map_body_cardinality_transfer(&output, || parent);
            prop_assert_eq!(
                actual,
                if output_kind == 3 { RowCardinalityProof::StaticPlural } else { parent }
            );
        }
    }
}
