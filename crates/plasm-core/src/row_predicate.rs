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
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One row-local comparison clause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowComparison {
    pub field: String,
    pub op: CompOp,
    pub value: TypedComparisonValue,
}

/// Flat AND of comparisons applied to materialized row JSON (v1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RowPredicate(pub Vec<RowComparison>);

/// Type-check context for row filters against a catalog entity schema.
pub struct RowPredicateTypeCtx<'a> {
    pub qe: &'a QualifiedEntityKey,
    pub cgs: &'a CGS,
    pub symbol_map: Option<&'a dyn SymbolSession>,
}

/// Parse a comma-separated predicate body by reusing the path parser on `Entity{body}`.
///
/// `row_schema_fields` is RA-2 `current_row_schema`. When non-empty (after `| select`),
/// predicate names bind that grain — including aliases that are not catalog fields.
pub fn parse_row_predicate_list(
    entity: &str,
    body: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
    row_schema_fields: &[String],
) -> Result<RowPredicate, String> {
    // Deterministic rewrite of Kusto / wire-shaped temporal RHS before parse
    // (`now() - 7d` → `7d ago`, etc.). Wire slots still pass `now-7d` unchanged.
    let rewritten = crate::temporal::rewrite_temporal_aliases_in_predicate_body(body);
    let input = format!("{entity}{{{}}}", rewritten.trim());
    let parsed =
        crate::expr_parser::parse_row_filter_body(&input, layers, sym_map, row_schema_fields)
            .map_err(|e| format!("row filter parse: {e}"))?;
    row_predicate_from_expr(&parsed.expr)
}

pub fn row_predicate_from_expr(expr: &Expr) -> Result<RowPredicate, String> {
    match expr {
        Expr::Query(q) => row_predicate_from_optional_predicate(q.predicate.as_ref()),
        _ => Err(
            "row filter requires field comparisons: use `rows | where field = value` with fields present on those rows. Keep catalog selection on `e#{wire=value}`."
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
        let pred =
            parse_row_predicate_list("LangItem", r#"owner="a", score>1"#, &stack, sym_map, &[])
                .expect("parse");
        assert_eq!(pred.0.len(), 2);
        assert_eq!(pred.0[0].field, "owner");
        assert_eq!(pred.0[1].field, "score");
    }

    fn digit_id_layers() -> (Arc<CGS>, Vec<std::sync::Arc<CGS>>) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/digit_id_identity");
        let cgs = Arc::new(load_schema_dir(&dir).expect("digit_id_identity"));
        (cgs.clone(), vec![cgs])
    }

    /// digit_id: unquoted i64 literal RA-8 coerces to exact digit string (not f64).
    #[test]
    fn parse_row_filter_eq_unquoted_pan_coerces_to_digit_id_string() {
        let (cgs, _layers_arc) = digit_id_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred =
            parse_row_predicate_list("DigitAccount", "pan=6419671322388907", &stack, sym_map, &[])
                .expect("unquoted digits on digit_id coerce via exact i64");
        assert_eq!(
            pred.0[0].value.typed_literal(),
            Some(&crate::TypedLiteral::String("6419671322388907".into()))
        );
    }

    /// Taught form: quoted digits on digit_id stay exact digit-string identity.
    #[test]
    fn parse_row_filter_eq_quoted_pan_is_digit_id_string() {
        let (cgs, _layers_arc) = digit_id_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred = parse_row_predicate_list(
            "DigitAccount",
            r#"pan="6419671322388907""#,
            &stack,
            sym_map,
            &[],
        )
        .expect("quoted digits on digit_id");
        assert_eq!(
            pred.0[0].value.typed_literal(),
            Some(&crate::TypedLiteral::String("6419671322388907".into()))
        );
    }

    /// Unquoted integer beyond f64 exact range still becomes the exact digit string.
    #[test]
    fn parse_row_filter_eq_beyond_f64_pan_stays_exact_digits() {
        let (cgs, _layers_arc) = digit_id_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred =
            parse_row_predicate_list("DigitAccount", "pan=9007199254740993", &stack, sym_map, &[])
                .expect("i64 beyond f64 exact range");
        assert_eq!(
            pred.0[0].value.typed_literal(),
            Some(&crate::TypedLiteral::String("9007199254740993".into()))
        );
        assert_ne!(
            9_007_199_254_740_993i64 as f64 as i64,
            9_007_199_254_740_993i64
        );
    }

    #[test]
    fn parse_row_filter_eq_float_on_digit_id_is_rejected() {
        let (cgs, _layers_arc) = digit_id_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let err = parse_row_predicate_list(
            "DigitAccount",
            "pan=9007199254740993.0",
            &stack,
            sym_map,
            &[],
        )
        .expect_err("IEEE float is not digit_id identity");
        let msg = err.to_string();
        assert!(
            msg.contains("digit_id") || msg.contains("float") || msg.contains("IEEE"),
            "expected float reject, got {msg}"
        );
    }

    #[test]
    fn parse_row_filter_on_id_field_is_predicate_not_get() {
        // Regression (WS2): `CompoundBranch` has `id_field: name`. Filtering `.filter{name="main"}`
        // on a materialized list must parse as a row predicate. Previously the `id_field → Get`
        // sugar folded `{name="main"}` into an `Expr::Get`, tripping the row-predicate contract with
        // the misleading "no list to filter" diagnostic.
        let (cgs, _layers_arc) = matrix_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred =
            parse_row_predicate_list("CompoundBranch", r#"name="main""#, &stack, sym_map, &[])
                .expect("row filter on id_field must parse as a predicate");
        assert_eq!(pred.0.len(), 1);
        assert_eq!(pred.0[0].field, "name");
        assert_eq!(pred.0[0].op, CompOp::Eq);
    }

    #[test]
    fn parse_row_filter_rejects_or() {
        let (cgs, _layers_arc) = matrix_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let err = parse_row_predicate_list(
            "LangItem",
            "owner=\"a\" or owner=\"b\"",
            &stack,
            sym_map,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("OR") || err.contains("parse"), "{err}");
    }

    #[test]
    fn parse_row_filter_binds_select_alias_on_current_row_schema() {
        let (cgs, _layers_arc) = matrix_layers();
        let stack = vec![CgsLayer::unset(cgs.as_ref())];
        let (full, _) = entity_slices_for_render(cgs.as_ref(), FocusSpec::All);
        let sym_map = Arc::new(SymbolMap::build(cgs.as_ref(), &full));
        let pred = parse_row_predicate_list(
            "LangItem",
            r#"handle="alice""#,
            &stack,
            sym_map.clone(),
            &[String::from("handle")],
        )
        .expect("RA-2: select alias must parse as a row predicate");
        assert_eq!(pred.0.len(), 1);
        assert_eq!(pred.0[0].field, "handle");
        assert_eq!(pred.0[0].op, CompOp::Eq);

        let err = parse_row_predicate_list(
            "LangItem",
            r#"handle="alice""#,
            &stack,
            sym_map.clone(),
            &[],
        )
        .unwrap_err();
        assert!(
            err.contains("handle") && err.contains("LangItem"),
            "empty grain must still reject a non-entity alias: {err}"
        );

        let err = parse_row_predicate_list(
            "LangItem",
            r#"owner="alice""#,
            &stack,
            sym_map,
            &[String::from("handle")],
        )
        .unwrap_err();
        assert!(
            err.contains("RA-2") && err.contains("handle"),
            "projected grain must name the taught alias, not synthesize Entity{{owner=}}: {err}"
        );
    }
}
