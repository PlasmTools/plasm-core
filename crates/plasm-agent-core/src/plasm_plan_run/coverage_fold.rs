//! Fold plan-node coverage across compute collection dependencies.
//!
//! [`ResultCoverage::combine*` ](plasm_runtime::ResultCoverage) lives in `plasm-runtime`.
//! This module owns agent-core membership / union dependency discovery used when stamping
//! synthetic compute coverage.
//!
//! **BindingSymbol residual:** membership RHS labels are still resolved via
//! [`PlanValue::BindingSymbol`](crate::plasm_plan::PlanValue::BindingSymbol) string labels →
//! [`PlanNodeId`] lookup. A missing label folds as [`ResultCoverage::Unknown`] (no typed IR
//! collection-deps in this pass). Declared pure/compute *sources* that are absent from
//! `materialized` are hard errors — see [`coverage_of_declared_source`].

use std::collections::BTreeMap;

use crate::plasm_plan::{ComputeOp, PlanNodeId};

use super::MaterializedNode;

/// Fold coverage across the primary source and every collection dependency the op
/// consumes (union RHS, membership `in` / `not in` bindings).
///
/// Complete left ∪ partial/unknown right must not stamp Complete. Limit / take
/// preserves upstream uncertainty in [`super::materialize_synthetic_node`].
///
/// BindingSymbol labels absent from `materialized` contribute [`ResultCoverage::Unknown`]
/// (residual string→node lookup; not a silent Complete).
#[must_use]
pub(crate) fn coverage_from_compute_collections(
    source_coverage: plasm_runtime::ResultCoverage,
    op: &ComputeOp,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> plasm_runtime::ResultCoverage {
    let dep_coverages = crate::plan_node_graph::compute_binding_labels(op)
        .into_iter()
        .map(|label| {
            PlanNodeId::new(label)
                .ok()
                .and_then(|id| materialized.get(&id))
                .map(|m| m.result.coverage)
                .unwrap_or(plasm_runtime::ResultCoverage::Unknown)
        });
    plasm_runtime::ResultCoverage::combine_all(
        std::iter::once(source_coverage).chain(dep_coverages),
    )
}

/// Coverage for a declared pure-step source.
///
/// - `None` (true constants / no upstream) → Complete.
/// - `Some(src)` missing from `materialized` → **error** (never silent Complete).
pub(crate) fn coverage_of_declared_source(
    source: Option<&PlanNodeId>,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<plasm_runtime::ResultCoverage, String> {
    match source {
        None => Ok(plasm_runtime::ResultCoverage::Complete),
        Some(src) => materialized
            .get(src)
            .map(|m| m.result.coverage)
            .ok_or_else(|| format!("source node {:?} has not been materialized", src.as_str())),
    }
}

/// Fold iterate_until seed (+ retained step) coverages.
///
/// The step `take` bound is not a Limit row-take: until-success does not promote Partial /
/// Unknown seed to Complete. Explicit Limit preserves input coverage through
/// [`plasm_runtime::coverage_after_explicit_take`] on synthetic compute only.
#[must_use]
pub(crate) fn coverage_for_iterate_until(
    seed: plasm_runtime::ResultCoverage,
    step_coverages: impl IntoIterator<Item = plasm_runtime::ResultCoverage>,
) -> plasm_runtime::ResultCoverage {
    plasm_runtime::ResultCoverage::combine_all(std::iter::once(seed).chain(step_coverages))
}

#[cfg(test)]
mod coverage_collection_tests {
    use super::*;
    use crate::plasm_plan::ComputeOp;
    use plasm_core::{FieldPath, OutputName};
    use plasm_runtime::{
        ExecutionResult, ExecutionSource, ExecutionStats, MaterializedRowSource, OperationLedger,
        ResultCoverage,
    };
    use std::sync::Arc;

    fn mat_with_coverage(id: &str, coverage: ResultCoverage) -> (PlanNodeId, MaterializedNode) {
        let node = PlanNodeId::new(id.to_string()).expect("node id");
        let mat = MaterializedNode {
            qualified_entity: crate::plasm_plan::QualifiedEntityKey {
                entry_id: "test".to_string(),
                entity: format!("PlanComputed_{id}"),
            },
            result: Arc::new(
                ExecutionResult {
                    count: 0,
                    entities: Vec::new(),
                    has_more: false,
                    coverage: ResultCoverage::Unknown,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats::default(),
                    request_fingerprints: vec![],
                    operations: OperationLedger::empty(),
                }
                .with_coverage(coverage),
            ),
            row_source: MaterializedRowSource::Inline(Vec::new()),
            row_identities: Vec::new(),
            artifact: None,
            display: id.to_string(),
            projection: None,
        };
        (node, mat)
    }

    #[test]
    fn union_complete_with_partial_secondary_is_partial() {
        let mut materialized = BTreeMap::new();
        let (right_id, right) = mat_with_coverage("right", ResultCoverage::Partial);
        materialized.insert(right_id, right);
        let op = ComputeOp::Union {
            other: OutputName::new("right").expect("output name"),
        };
        assert_eq!(
            coverage_from_compute_collections(ResultCoverage::Complete, &op, &materialized),
            ResultCoverage::Partial
        );
    }

    #[test]
    fn union_complete_with_unknown_secondary_is_unknown() {
        let mut materialized = BTreeMap::new();
        let (right_id, right) = mat_with_coverage("right", ResultCoverage::Unknown);
        materialized.insert(right_id, right);
        let op = ComputeOp::Union {
            other: OutputName::new("right").expect("output name"),
        };
        assert_eq!(
            coverage_from_compute_collections(ResultCoverage::Complete, &op, &materialized),
            ResultCoverage::Unknown
        );
    }

    #[test]
    fn membership_not_in_complete_with_partial_rhs_is_partial() {
        let mut materialized = BTreeMap::new();
        let (rhs_id, rhs) = mat_with_coverage("excluded", ResultCoverage::Partial);
        materialized.insert(rhs_id, rhs);
        let op = ComputeOp::Filter {
            predicates: vec![crate::plasm_plan::PlanPredicate {
                field_path: FieldPath::from_dotted("id").expect("field"),
                op: crate::plasm_plan::PlanPredicateOp::NotIn,
                value: crate::plasm_plan::PlanValue::BindingSymbol {
                    binding: "excluded".into(),
                    path: vec!["id".into()],
                },
            }]
            .into(),
        };
        assert_eq!(
            coverage_from_compute_collections(ResultCoverage::Complete, &op, &materialized),
            ResultCoverage::Partial
        );
    }

    #[test]
    fn membership_not_in_complete_with_unknown_rhs_is_unknown() {
        let mut materialized = BTreeMap::new();
        let (rhs_id, rhs) = mat_with_coverage("excluded", ResultCoverage::Unknown);
        materialized.insert(rhs_id, rhs);
        let op = ComputeOp::Filter {
            predicates: vec![crate::plasm_plan::PlanPredicate {
                field_path: FieldPath::from_dotted("id").expect("field"),
                op: crate::plasm_plan::PlanPredicateOp::NotIn,
                value: crate::plasm_plan::PlanValue::BindingSymbol {
                    binding: "excluded".into(),
                    path: vec!["id".into()],
                },
            }]
            .into(),
        };
        assert_eq!(
            coverage_from_compute_collections(ResultCoverage::Complete, &op, &materialized),
            ResultCoverage::Unknown
        );
    }

    #[test]
    fn limit_preserves_uncertainty_after_combined_inputs() {
        assert_eq!(
            plasm_runtime::coverage_after_explicit_take(ResultCoverage::Partial, 3, 3),
            ResultCoverage::Partial
        );
    }

    #[test]
    fn declared_source_none_is_complete() {
        let materialized = BTreeMap::new();
        assert_eq!(
            coverage_of_declared_source(None, &materialized).expect("ok"),
            ResultCoverage::Complete
        );
    }

    #[test]
    fn declared_source_missing_is_error() {
        let materialized = BTreeMap::new();
        let id = PlanNodeId::new("missing_src").expect("id");
        let err = coverage_of_declared_source(Some(&id), &materialized).expect_err("must err");
        assert!(
            err.contains("has not been materialized"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn iterate_until_partial_seed_does_not_stamp_complete() {
        assert_eq!(
            coverage_for_iterate_until(ResultCoverage::Partial, []),
            ResultCoverage::Partial
        );
        assert_eq!(
            coverage_for_iterate_until(
                ResultCoverage::Partial,
                [ResultCoverage::Complete, ResultCoverage::Complete]
            ),
            ResultCoverage::Partial
        );
        assert_eq!(
            coverage_for_iterate_until(ResultCoverage::Unknown, [ResultCoverage::Complete]),
            ResultCoverage::Unknown
        );
    }
}
