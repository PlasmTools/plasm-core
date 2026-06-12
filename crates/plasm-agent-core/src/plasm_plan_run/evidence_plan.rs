//! Plan-node expression helpers for evidence `step_executed` / archive preimages.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Expr, Value};

/// Fallback parsed expr for synthetic plan nodes and archive preimages.
pub fn archive_fallback_parsed_expr() -> ParsedExpr {
    ParsedExpr {
        expr: Expr::TeachingValue {
            value: Value::String("__plasm_run_artifact_archive__".into()),
        },
        projection: None,
    }
}

pub fn parsed_expr_for_plan_node(node: &crate::plasm_plan::ValidatedPlanNode) -> ParsedExpr {
    match node {
        crate::plasm_plan::ValidatedPlanNode::Surface(surface) => surface
            .ir
            .as_ref()
            .map(|ir| ParsedExpr {
                expr: ir.expr.clone(),
                projection: ir.projection.clone(),
            })
            .unwrap_or_else(archive_fallback_parsed_expr),
        crate::plasm_plan::ValidatedPlanNode::RelationTraversal(relation) => ParsedExpr {
            expr: relation.relation.ir.expr.clone(),
            projection: relation.relation.ir.projection.clone(),
        },
        crate::plasm_plan::ValidatedPlanNode::ForEach(for_each) => {
            if let Ok(expr) =
                serde_json::from_value::<plasm_core::Expr>(for_each.effect_template.ir_template.expr.clone())
            {
                ParsedExpr {
                    expr,
                    projection: if for_each.effect_template.projection.is_empty() {
                        None
                    } else {
                        Some(for_each.effect_template.projection.clone())
                    },
                }
            } else {
                archive_fallback_parsed_expr()
            }
        }
        _ => archive_fallback_parsed_expr(),
    }
}
