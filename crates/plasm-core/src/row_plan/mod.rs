//! Fused row-compute IR and engine ports.
//!
//! [`ComputeOp`](crate::ComputeOp) remains the hashed PlasmComp constructor. This module is the
//! execute-time IR. Polars types do not appear here.

mod collect;
mod engine;
mod error;
mod expr;
mod filter;
mod fold;
mod ids;
mod plan;
mod schema;
mod with_parse;

pub use collect::{CollectCardinality, CollectReason, PageCursor, PageSize, RenderCollectSpec};
pub use engine::{
    CollectRows, CollectedFrame, CompileRowPlan, IngestBatch, IngestRows, RowComputeEngine,
    ScanSource,
};
pub use error::{
    CollectError, FrameSchemaError, FusionError, RowComputeError, RowFilterError, RowTypeError,
    ScanError,
};
pub use expr::{ArithOp, ProjectSpec, WithColumn, WithExpr, WithExprError, WithLiteral};
pub use filter::{CatalogFilter, RowFilter};
pub use fold::{fold_compute_ops, plan_node_from_compute};
pub use ids::{EnginePlanId, FixtureScanId, FrameId, GraphSnapshotId, RowNodeId, SurfaceMeaningId};
pub use plan::{MoneyAggLaw, NumericAgg, Pipeline, PlanNode, RowPlan, TypedAggregate};
pub use schema::{
    ColumnName, FrameShape, IdentityPreservation, LogicalColumn, LogicalColumnType,
    MoneyColumnLayout, PlasmFrameSchema, RemapReason,
};
pub use with_parse::parse_with_body;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_monad::payload::PlasmDataValue;
    use crate::plasm_monad::{ComputeOp, FieldPath, OutputName, PlanPredicate, PlanPredicateOp};

    #[test]
    fn plan_node_has_no_render_or_join_variants() {
        let names: Vec<&str> = vec![
            "Filter",
            "Sort",
            "Limit",
            "Dedupe",
            "Distinct",
            "Project",
            "With",
            "GroupBy",
            "Aggregate",
        ];
        assert!(!names.contains(&"Render"));
        assert!(!names.contains(&"EquiJoin"));
        assert!(!names.contains(&"Join"));
    }

    #[test]
    fn with_parse_now_minus_and_mul() {
        let cols = parse_with_body("age_days: (now - updated_at), notional: quantity * price")
            .expect("parse");
        assert_eq!(cols.len(), 2);
        match &cols[0].expr {
            WithExpr::Arith {
                op: ArithOp::Sub,
                lhs,
                rhs,
            } => {
                assert!(matches!(lhs.as_ref(), WithExpr::Now));
                assert!(matches!(rhs.as_ref(), WithExpr::Field(_)));
            }
            other => panic!("expected now - field, got {other:?}"),
        }
        assert!(matches!(
            cols[1].expr,
            WithExpr::Arith {
                op: ArithOp::Mul,
                ..
            }
        ));
    }

    #[test]
    fn with_parse_div_concat_when_and_field_minus_field() {
        let cols = parse_with_body(
            "cycle: (updated_at - created_at), rate: qty / n, name: first + last, blank: when(len(title)=0, 1, 0), stale: when(now - updated_at > 14, 1, 0)",
        )
        .expect("parse");
        assert_eq!(cols.len(), 5);
        assert!(matches!(
            &cols[0].expr,
            WithExpr::Arith {
                op: ArithOp::Sub,
                lhs,
                rhs,
            } if matches!(lhs.as_ref(), WithExpr::Field(_)) && matches!(rhs.as_ref(), WithExpr::Field(_))
        ));
        assert!(matches!(
            cols[1].expr,
            WithExpr::Arith {
                op: ArithOp::Div,
                ..
            }
        ));
        assert!(matches!(
            cols[2].expr,
            WithExpr::Arith {
                op: ArithOp::Add,
                ..
            }
        ));
        match &cols[3].expr {
            WithExpr::When { lhs, op, rhs, .. } => {
                assert!(matches!(lhs.as_ref(), WithExpr::Len { .. }));
                assert_eq!(*op, PlanPredicateOp::Eq);
                assert!(matches!(
                    rhs.as_ref(),
                    WithExpr::Literal(WithLiteral::Integer(0))
                ));
            }
            other => panic!("expected when(len), got {other:?}"),
        }
        match &cols[4].expr {
            WithExpr::When { lhs, op, .. } => {
                assert!(matches!(
                    lhs.as_ref(),
                    WithExpr::Arith {
                        op: ArithOp::Sub,
                        ..
                    }
                ));
                assert_eq!(*op, PlanPredicateOp::Gt);
            }
            other => panic!("expected when(now - field), got {other:?}"),
        }
    }

    #[test]
    fn fold_render_is_collect_barrier() {
        let op = ComputeOp::Render {
            columns: vec![OutputName::new("title").unwrap()],
            template: "{{ r.title }}".into(),
            column_aliases: Default::default(),
            render_bindings: vec![],
        };
        let plan = fold_compute_ops(
            &[op],
            FrameId::new(1),
            crate::plasm_monad::StepId::new("out").unwrap(),
            CollectCardinality::List,
        )
        .unwrap();
        assert!(matches!(plan.collect(), CollectReason::Render { .. }));
        assert!(plan.nodes().is_empty());
    }

    #[test]
    fn fold_filter_does_not_become_catalog_filter() {
        let pred = PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::Eq,
            value: PlasmDataValue::Literal {
                value: serde_json::json!("alice"),
            },
        };
        let node = plan_node_from_compute(&ComputeOp::Filter {
            predicates: vec![pred],
        })
        .unwrap();
        assert!(matches!(node, PlanNode::Filter(_)));
    }

    #[test]
    fn with_body_rejects_empty() {
        assert!(parse_with_body("").is_err());
        assert!(parse_with_body("   ").is_err());
    }

    #[test]
    fn with_body_rejects_hop_summary_and_unknown_calls() {
        let err = parse_with_body("n: count(r1)").unwrap_err().to_string();
        assert!(err.contains("count"), "{err}");
        assert!(parse_with_body("x: open(labels)").is_err());
        assert!(parse_with_body("x: rank(score)").is_err());
        let age = parse_with_body("x: age_days(updated_at)")
            .unwrap_err()
            .to_string();
        assert!(age.contains("age_days"), "{age}");
        assert!(age.contains("len"), "{age}");
        assert!(age.contains("when"), "{age}");
        assert!(!age.contains("age_days, len"), "{age}");
        let empty = parse_with_body("x: empty(title)").unwrap_err().to_string();
        assert!(empty.contains("empty"), "{empty}");
        assert!(parse_with_body("x: datediff(updated_at)").is_err());
        assert!(parse_with_body("x: col(updated_at)").is_err());
    }

    #[test]
    fn fusion_error_join_from_surface_is_named() {
        let msg = FusionError::JoinFromSurface.to_string();
        assert!(msg.contains("join"));
        assert!(msg.contains("surface"));
    }
}
