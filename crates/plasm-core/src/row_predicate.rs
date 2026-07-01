//! Row-local filter predicates (artifact plane): flat AND of field comparisons.
//!
//! Parsed from `.filter{…}` / `.filter(…)` bodies via the same surface grammar as entity
//! brace queries, but restricted to comparisons only (no `ExistsRelation`, OR/NOT).

use crate::cgs_federation::{CgsLayer, QualifiedEntityKey};
use crate::predicate::Predicate;
use crate::schema::{EntityDef, CGS};
use crate::symbol_tuning::SymbolSession;
use crate::type_checker::type_check_predicate;
use crate::{CompOp, Expr, TypeError, TypedComparisonValue};
use std::sync::Arc;

/// One row-local comparison clause.
#[derive(Debug, Clone, PartialEq)]
pub struct RowComparison {
    pub field: String,
    pub op: CompOp,
    pub value: TypedComparisonValue,
}

/// Flat AND of comparisons applied to materialized row JSON (v1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowPredicate(pub Vec<RowComparison>);

/// Type-check context for row filters against a catalog entity schema.
pub struct RowPredicateTypeCtx<'a> {
    pub qe: &'a QualifiedEntityKey,
    pub cgs: &'a CGS,
    pub symbol_map: Option<&'a dyn SymbolSession>,
}

/// Parse a comma-separated predicate body by reusing the path parser on `Entity{body}`.
pub fn parse_row_predicate_list(
    entity: &str,
    body: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
) -> Result<RowPredicate, String> {
    let input = format!("{entity}{{{}}}", body.trim());
    let parsed = crate::expr_parser::parse_with_cgs_layers(&input, layers, sym_map)
        .map_err(|e| format!("row filter parse: {e}"))?;
    row_predicate_from_expr(&parsed.expr)
}

pub fn row_predicate_from_expr(expr: &Expr) -> Result<RowPredicate, String> {
    match expr {
        Expr::Query(q) => row_predicate_from_optional_predicate(q.predicate.as_ref()),
        _ => Err(
            "row filter body must be brace predicate comparisons (`{p#=…, p#>…}`). Row `.filter{…}` runs on a materialized list — bind `label = e#` then `label.filter{…}`; a GET-only or search-only entity has no list to filter (use scoped `e#{p#=…}` or `e#~\"text\"` instead)."
                .into(),
        ),
    }
}

fn row_predicate_from_optional_predicate(pred: Option<&Predicate>) -> Result<RowPredicate, String> {
    match pred {
        None => Ok(RowPredicate(vec![])),
        Some(p) => Ok(RowPredicate(flatten_flat_and(p)?)),
    }
}

fn flatten_flat_and(pred: &Predicate) -> Result<Vec<RowComparison>, String> {
    match pred {
        Predicate::True => Ok(vec![]),
        Predicate::False => Err("row filter: `false` is not allowed".into()),
        Predicate::Comparison { field, op, value } => Ok(vec![RowComparison {
            field: field.clone(),
            op: *op,
            value: value.clone(),
        }]),
        Predicate::And { args } => {
            let mut out = Vec::new();
            for arg in args {
                out.extend(flatten_flat_and(arg)?);
            }
            Ok(out)
        }
        Predicate::Or { .. } => {
            Err("row filter: OR is not supported (use comma-separated AND)".into())
        }
        Predicate::Not { .. } => Err("row filter: NOT is not supported".into()),
        Predicate::ExistsRelation { .. } => {
            Err("row filter: relation exists predicates are not supported".into())
        }
    }
}

/// Type-check row comparisons against the owning entity (empty capability params).
pub fn type_check_row_predicate(
    pred: &RowPredicate,
    ctx: &RowPredicateTypeCtx<'_>,
) -> Result<(), TypeError> {
    let entity =
        ctx.cgs
            .get_entity(ctx.qe.entity.as_str())
            .ok_or_else(|| TypeError::EntityNotFound {
                entity: ctx.qe.entity.to_string(),
            })?;
    for clause in &pred.0 {
        let p = Predicate::comparison(clause.field.as_str(), clause.op, clause.value.clone());
        type_check_predicate(&p, entity, &[], ctx.cgs)?;
    }
    Ok(())
}

/// Resolved entity definition for downstream field-path validation.
pub fn entity_def_for_row_predicate<'a>(
    ctx: &RowPredicateTypeCtx<'a>,
) -> Result<&'a EntityDef, TypeError> {
    ctx.cgs
        .get_entity(ctx.qe.entity.as_str())
        .ok_or_else(|| TypeError::EntityNotFound {
            entity: ctx.qe.entity.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::{entity_slices_for_render, FocusSpec, SymbolMap};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn matrix_layers() -> (Arc<CGS>, Vec<std::sync::Arc<CGS>>) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(load_schema_dir(&dir).expect("matrix"));
        (cgs.clone(), vec![cgs])
    }

    #[test]
    fn parse_row_filter_flat_and() {
        let (cgs, _layers_arc) = matrix_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred = parse_row_predicate_list("LangItem", r#"owner="a", score>1"#, &stack, sym_map)
            .expect("parse");
        assert_eq!(pred.0.len(), 2);
        assert_eq!(pred.0[0].field, "owner");
        assert_eq!(pred.0[1].field, "score");
    }

    #[test]
    fn parse_row_filter_rejects_or() {
        let (cgs, _layers_arc) = matrix_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let err =
            parse_row_predicate_list("LangItem", "owner=\"a\" or owner=\"b\"", &stack, sym_map)
                .unwrap_err();
        assert!(err.contains("OR") || err.contains("parse"), "{err}");
    }
}
