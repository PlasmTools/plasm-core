//! Surface splitting for **query** `Entity{ … }` predicate bodies.
//!
//! # Same scan as program lines
//!
//! Comma boundaries use [`super::program_surface::split_top_level`]. Heredoc open/skip uses
//! [`super::heredoc_surface::heredoc_surface_step_at`], matching other surface scanners so
//! correction and staging never disagree on where delimiters fall.
//!
//! # Semantic delta vs the strict path parser
//!
//! This module is **not** a second grammar: it is a **string surface** helper for
//! [`crate::expr_correction::try_auto_correct`]. Do not conflate it with the strict path on
//! [`Parser`](crate::expr_parser::Parser) (`parse_pred`, `parse_op`, `parse_preds` in
//! `expr_parser/mod.rs` — implementation methods, not a separate grammar surface):
//!
//! - **No CGS** — no field/capability validation, no `PredicateFieldNotFound`, no `Value` typing
//!   or temporal coercion.
//! - **Clauses** are split on the first **top-level** comparison operator only. **Two-byte
//!   operators** (`!=`, `>=`, `<=`) are tried **before** single-byte (`=`, `~`, `>`, `<`) at the
//!   same offset, matching `Parser::parse_op` in `expr_parser/mod.rs`.
//! - A clause with **no** such operator, or with an **empty** field or value after split, is
//!   **skipped** (not an error). That differs from strict parse, which rejects malformed preds.
//! - **teaching table / omitted RHS** (`field,` with null value) is not modelled; strict parse accepts a
//!   null RHS when the next token is `,` or `}` (see `Parser::parse_predicate_rhs_after_op` in
//!   `expr_parser/mod.rs`).
//!
//! The **stable** internal contract for tests and readers is
//! [`parse_loose_query_predicate_body`] (and [`split_query_brace_form`] for the outer shape).
//! [`find_top_level_comparison_op`] is `pub(crate)` only — do not re-export as a public oracle.

use super::heredoc_surface::{heredoc_surface_step_at, HeredocSurfaceStep};
use super::program_surface::split_top_level;

/// `Entity{preds}` where `Entity` has no `(` or `.` — i.e. query brace form, not get/chain.
///
/// Returns `Some((entity, body))` with `body` the inner text between `{` and the matching final `}`.
pub fn split_query_brace_form(input: &str) -> Option<(&str, &str)> {
    let input = input.trim();
    let brace = input.find('{')?;
    let entity = input[..brace].trim();
    if !input.ends_with('}') {
        return None;
    }
    let body = &input[brace + 1..input.len() - 1];
    if entity.is_empty() || entity.contains('(') || entity.contains('.') {
        return None;
    }
    Some((entity, body))
}

/// One predicate clause split for lexicon correction (`field op value` / `Ent.field op value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateSurfaceClause {
    pub raw: String,
    pub field: String,
    pub op: String,
    pub value: String,
}

fn split_predicate_clauses(body: &str) -> Result<Vec<&str>, String> {
    split_top_level(body.trim(), ',')
}

/// Find the first comparison operator at nesting depth 0 (outside quotes and heredocs).
///
/// Tries `!=`, `>=`, `<=` before any single-byte operator at the same offset, in line with
/// [`super::Parser::parse_op`].
pub(crate) fn find_top_level_comparison_op(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut quote = None::<char>;

    while i < b.len() {
        let c = s[i..].chars().next()?;
        let cl = c.len_utf8();

        if quote.is_none() && paren == 0 && bracket == 0 && brace == 0 {
            match heredoc_surface_step_at(s, i) {
                Ok(HeredocSurfaceStep::SkipTo(next)) => {
                    i = next;
                    continue;
                }
                Ok(HeredocSurfaceStep::OpenerIncomplete { .. }) => {
                    return None;
                }
                Ok(HeredocSurfaceStep::NotAnOpener) | Err(_) => {}
            }
        }

        if let Some(q) = quote {
            if c == '\\' && q == '"' && i + cl < b.len() {
                i += cl + s[i + cl..].chars().next()?.len_utf8();
                continue;
            }
            if c == q {
                quote = None;
            }
            i += cl;
            continue;
        }

        match c {
            '"' | '\'' => {
                quote = Some(c);
                i += cl;
                continue;
            }
            '(' => {
                paren += 1;
                i += cl;
                continue;
            }
            ')' => {
                paren -= 1;
                i += cl;
                continue;
            }
            '[' => {
                bracket += 1;
                i += cl;
                continue;
            }
            ']' => {
                bracket -= 1;
                i += cl;
                continue;
            }
            '{' => {
                brace += 1;
                i += cl;
                continue;
            }
            '}' => {
                brace -= 1;
                i += cl;
                continue;
            }
            _ => {}
        }

        if paren == 0 && bracket == 0 && brace == 0 {
            if i + 1 < b.len() {
                let two = &b[i..i + 2];
                if two == b"!=" || two == b">=" || two == b"<=" {
                    return Some((i, 2));
                }
            }
            if matches!(b.get(i).copied(), Some(b'=' | b'~' | b'>' | b'<')) {
                return Some((i, 1));
            }
        }

        i += cl;
    }
    None
}

/// Parse loose predicate clauses from a brace body (comma split + operator scan).
pub fn parse_loose_query_predicate_body(body: &str) -> Result<Vec<PredicateSurfaceClause>, String> {
    let mut out = Vec::new();
    for part in split_predicate_clauses(body)? {
        let raw = part.trim();
        if raw.is_empty() {
            continue;
        }
        let Some((op_idx, op_len)) = find_top_level_comparison_op(raw) else {
            continue;
        };
        let field = raw[..op_idx].trim().to_string();
        let op = raw[op_idx..op_idx + op_len].to_string();
        let value = raw[op_idx + op_len..].trim().to_string();
        if field.is_empty() || value.is_empty() {
            continue;
        }
        out.push(PredicateSurfaceClause {
            raw: raw.to_string(),
            field,
            op,
            value,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_query_brace_form_basic() {
        let r = split_query_brace_form("Space{team_id=Team(42)}");
        assert_eq!(r, Some(("Space", "team_id=Team(42)")));
    }

    #[test]
    fn split_query_brace_form_rejects_get_and_chain() {
        assert!(split_query_brace_form("Space(42)").is_none());
        assert!(split_query_brace_form("Space(42).folders").is_none());
    }

    #[test]
    fn parse_loose_basic() {
        let preds =
            parse_loose_query_predicate_body("space_id=Space(42), archived=true").expect("ok");
        assert_eq!(preds.len(), 2);
        assert_eq!(preds[0].field, "space_id");
        assert_eq!(preds[0].value, "Space(42)");
        assert_eq!(preds[1].field, "archived");
        assert_eq!(preds[1].value, "true");
    }

    #[test]
    fn comma_inside_entity_ctor_does_not_split() {
        let preds = parse_loose_query_predicate_body("x=Foo(a,b), y=2").expect("ok");
        assert_eq!(preds.len(), 2);
        assert_eq!(preds[0].field, "x");
        assert_eq!(preds[0].value, "Foo(a,b)");
    }

    #[test]
    fn op_inside_double_quotes_ignored() {
        let preds = parse_loose_query_predicate_body(r#"k="a=b""#).expect("ok");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].field, "k");
        assert_eq!(preds[0].value, r#""a=b""#);
    }
}
