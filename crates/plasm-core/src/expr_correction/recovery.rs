use crate::domain_lexicon::DomainLexicon;
use crate::expr_parser;
use crate::CGS;

use super::auto_correct::{try_auto_correct, CorrectionOutcome};
use super::entity_case::try_normalize_entity_case;
use super::RecoveryHint;

/// After [`expr_parser::parse`] fails on `original`, apply [`try_normalize_entity_case`]
/// then [`try_auto_correct`]. Returns the recovered [`ParsedExpr`] or the best-effort
/// [`ParseError`] against the **normalized** `work` string, that `work` for diagnostics,
/// and extra hints when [`CorrectionOutcome::Ambiguous`].
///
/// When recovery succeeds, [`recover_parse_with_rewrite`] also returns `Some(resolved)`
/// iff the expression string that parsed differs from `original` (entity-case or lexicon
/// rewrite) so callers can surface deterministic correction in reports.
#[allow(clippy::type_complexity, clippy::result_large_err)]
pub fn recover_parse_with_rewrite(
    original: &str,
    cgs: &CGS,
    lexicon: &DomainLexicon,
) -> Result<
    (expr_parser::ParsedExpr, Option<String>),
    (expr_parser::ParseError, String, Vec<RecoveryHint>),
> {
    let work = try_normalize_entity_case(original, cgs).unwrap_or_else(|| original.to_string());
    if let Ok(p) = expr_parser::parse(&work, cgs) {
        let resolved = (work != original).then_some(work);
        return Ok((p, resolved));
    }
    match try_auto_correct(&work, lexicon, cgs) {
        CorrectionOutcome::Corrected(s) | CorrectionOutcome::Dropped(s) => {
            match expr_parser::parse(&s, cgs) {
                Ok(p) => {
                    let resolved = (s != original).then_some(s);
                    Ok((p, resolved))
                }
                Err(e) => Err((e, work, Vec::new())),
            }
        }
        CorrectionOutcome::Ambiguous { hints } => {
            let e = expr_parser::parse(&work, cgs).unwrap_err();
            Err((e, work, hints))
        }
        CorrectionOutcome::Uncorrectable => match expr_parser::parse(&work, cgs) {
            Ok(p) => {
                let resolved = (work != original).then_some(work);
                Ok((p, resolved))
            }
            Err(e) => Err((e, work, Vec::new())),
        },
    }
}

#[allow(clippy::result_large_err)]
pub fn recover_parse(
    original: &str,
    cgs: &CGS,
    lexicon: &DomainLexicon,
) -> Result<expr_parser::ParsedExpr, (expr_parser::ParseError, String, Vec<RecoveryHint>)> {
    recover_parse_with_rewrite(original, cgs, lexicon).map(|(p, _)| p)
}

/// In-grammar session parse when `sym_map` is set; otherwise wire [`expr_parser::parse`]. On failure,
/// same lexicon / entity-case recovery as [`recover_parse_with_rewrite`].
#[allow(clippy::type_complexity, clippy::result_large_err)]
pub fn parse_session_line_with_rewrite_recovery(
    original: &str,
    cgs: &CGS,
    lexicon: &DomainLexicon,
    sym_map: Option<std::sync::Arc<crate::symbol_tuning::SymbolMap>>,
) -> Result<
    (expr_parser::ParsedExpr, Option<String>),
    (expr_parser::ParseError, String, Vec<RecoveryHint>),
> {
    match expr_parser::parse_session_line(original, cgs, sym_map) {
        Ok(p) => Ok((p, None)),
        Err(_) => recover_parse_with_rewrite(original, cgs, lexicon),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_lexicon::DomainLexicon;
    use crate::Expr;

    #[test]
    fn recover_parse_unifies_eval_and_repl_pipeline() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let lexicon = DomainLexicon::from_cgs(&cgs);
        let p = recover_parse("pet{status=available}", &cgs, &lexicon).expect("recover");
        assert!(crate::expr_parser::parse("pet{status=available}", &cgs).is_err());
        assert!(matches!(p.expr, Expr::Query(_)));
    }

    #[test]
    fn recover_parse_with_rewrite_records_resolved_string() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let lexicon = DomainLexicon::from_cgs(&cgs);
        let (p, resolved) =
            recover_parse_with_rewrite("pet{status=available}", &cgs, &lexicon).expect("recover");
        assert!(resolved.is_some());
        assert!(resolved.unwrap().starts_with("Pet{"));
        assert!(matches!(p.expr, Expr::Query(_)));
    }
}
