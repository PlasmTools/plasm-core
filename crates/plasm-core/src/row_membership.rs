//! Row-plane set membership / difference (`| where field in rhs` / `not in`).
//!
//! RA-13: the RHS is a **closed one-column rowset** (binding or parenthesized pipeline),
//! not a brace/backend `in` and not a literal dest list.

use crate::expr_parser::{is_valid_program_label, parse_expr_node, split_top_level, RowExpr};

/// Membership atom parsed from a `| where` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMembership {
    pub field: String,
    pub rhs: MembershipRhs,
    /// `true` = `not in` (anti-join).
    pub anti: bool,
}

/// Closed rowset on the right of `in` / `not in`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipRhs {
    /// Already-bound one-column rowset.
    Binding(String),
    /// Parenthesized pipeline (compiled to an anonymous binding at lower time).
    Pipe(String),
}

/// Split a `| where` body on top-level commas (implicit AND).
pub fn split_where_and_clauses(body: &str) -> Result<Vec<&str>, String> {
    let parts = split_top_level(body.trim(), ',')?;
    let out: Vec<&str> = parts
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if out.is_empty() {
        return Err("where predicates must not be empty".into());
    }
    Ok(out)
}

/// Parse one clause as RA-13 membership, or `Ok(None)` when it is a scalar compare.
pub fn parse_membership_clause(clause: &str) -> Result<Option<RowMembership>, String> {
    let raw = clause.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let Some((field, anti, rhs_raw)) = split_membership_op(raw)? else {
        return Ok(None);
    };
    if !is_row_field_ident(&field) {
        return Err(format!(
            "membership LHS `{field}` must be a current-row field (RA-2 / RA-13)"
        ));
    }
    let rhs = parse_closed_rowset_ref(rhs_raw, "membership")?;
    Ok(Some(RowMembership { field, rhs, anti }))
}

fn is_row_field_ident(field: &str) -> bool {
    let mut chars = field.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Find top-level ` not in ` / ` in ` (outside quotes and nesting).
fn split_membership_op(raw: &str) -> Result<Option<(String, bool, &str)>, String> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<char>;
    while i < bytes.len() {
        let c = raw[i..].chars().next().expect("utf-8");
        let cl = c.len_utf8();
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            _ => {}
        }
        if quote.is_none() && depth == 0 && ident_boundary_before(raw, i) {
            if let Some(after) = strip_keyword_at(&raw[i..], "not") {
                let after = after.trim_start();
                if let Some(rhs) = strip_keyword_at(after, "in") {
                    let field = raw[..i].trim();
                    return Ok(Some((field.to_string(), true, rhs.trim())));
                }
            }
            if let Some(rhs) = strip_keyword_at(&raw[i..], "in") {
                let field = raw[..i].trim();
                if !field.is_empty() {
                    return Ok(Some((field.to_string(), false, rhs.trim())));
                }
            }
        }
        i += cl;
    }
    Ok(None)
}

fn ident_boundary_before(s: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    !s[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn strip_keyword_at<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(kw)?;
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    // Require a boundary before the keyword (start or whitespace/punct already consumed by caller).
    Some(rest)
}

/// Closed rowset on the right of RA-13 `in` / RA-14 `union`.
pub fn parse_closed_rowset_ref(rhs: &str, lane: &str) -> Result<MembershipRhs, String> {
    let rhs = rhs.trim();
    let ra = if lane == "union" { "RA-14" } else { "RA-13" };
    if rhs.is_empty() {
        return Err(format!(
            "{lane} RHS is empty; use a binding or `(other | select field)` ({ra})"
        ));
    }
    if rhs.starts_with('(') {
        if !rhs.ends_with(')') {
            return Err(format!(
                "{lane} RHS parenthesized pipeline is unclosed; write `(other | select field)` ({ra})"
            ));
        }
        let inner = rhs[1..rhs.len() - 1].trim();
        if inner.is_empty() {
            return Err(format!("{lane} RHS `(…)` is empty"));
        }
        if looks_like_literal_list(inner) {
            return Err(literal_list_reject(lane, inner));
        }
        let node =
            parse_expr_node(inner).map_err(|e| format!("{lane} RHS pipeline `{inner}`: {e}"))?;
        if node.apply.is_some() {
            return Err(format!(
                "{lane} RHS cannot take `=>`; bind the pipeline then use the label ({ra})"
            ));
        }
        if matches!(node.row, RowExpr::Iterate(_)) {
            return Err(format!("{lane} RHS cannot be `iterate … until`"));
        }
        return Ok(MembershipRhs::Pipe(inner.to_string()));
    }
    if looks_like_literal_list(rhs) {
        return Err(literal_list_reject(lane, rhs));
    }
    if !is_valid_program_label(rhs) {
        return Err(format!(
            "{lane} RHS `{rhs}` must be a binding or `(other | select field)` ({ra})"
        ));
    }
    Ok(MembershipRhs::Binding(rhs.to_string()))
}

fn looks_like_literal_list(inner: &str) -> bool {
    let t = inner.trim();
    t.starts_with('"') || t.starts_with('\'') || t.contains(',') && !t.contains('|')
}

fn literal_list_reject(lane: &str, rhs: &str) -> String {
    let mut msg = format!(
        "{lane} RHS must be a rowset (binding or parenthesized `| select` pipeline), not a literal list"
    );
    if let Some(quoted) = crate::unquote_single_string_literal(rhs) {
        if let Some(hint) = crate::quoted_literal_hint(quoted, None) {
            msg.push('\n');
            msg.push_str(&hint);
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_in_binding() {
        let m = parse_membership_clause("owner in peers")
            .expect("parse")
            .expect("membership");
        assert_eq!(m.field, "owner");
        assert!(!m.anti);
        assert_eq!(m.rhs, MembershipRhs::Binding("peers".into()));
    }

    #[test]
    fn parses_not_in_parenthesized_pipe() {
        let m = parse_membership_clause(
            r#"owner not in (LangItem | where owner = "alice" | select owner)"#,
        )
        .expect("parse")
        .expect("membership");
        assert_eq!(m.field, "owner");
        assert!(m.anti);
        assert!(matches!(m.rhs, MembershipRhs::Pipe(p) if p.contains("select owner")));
    }

    #[test]
    fn parenthesized_select_is_lawful_shape() {
        let m = parse_membership_clause("owner in (peers | select owner)")
            .expect("parse")
            .expect("membership");
        assert_eq!(m.field, "owner");
        assert!(!m.anti);
        assert_eq!(m.rhs, MembershipRhs::Pipe("peers | select owner".into()));
    }

    #[test]
    fn scalar_compare_is_none() {
        assert!(parse_membership_clause(r#"owner = "alice""#)
            .expect("parse")
            .is_none());
        assert!(parse_membership_clause("score >= 10")
            .expect("parse")
            .is_none());
    }

    #[test]
    fn rejects_literal_list() {
        let err = parse_membership_clause(r#"email in ("a", "b")"#).expect_err("list");
        assert!(err.contains("not a literal list"), "{err}");
    }

    #[test]
    fn quoted_string_rowset_fails_with_literal_hint() {
        let err = parse_membership_clause(r#"title in "item""#).expect_err("quoted rowset");
        assert!(err.contains("not a literal list"), "{err}");
        assert!(err.contains("string literal"), "{err}");
        assert!(err.contains("\"item\""), "{err}");
    }

    #[test]
    fn incoming_is_not_in_keyword() {
        assert!(parse_membership_clause(r#"incoming = true"#)
            .expect("parse")
            .is_none());
    }

    #[test]
    fn pin_field_does_not_steal_in() {
        let m = parse_membership_clause("pin in peers")
            .expect("parse")
            .expect("membership");
        assert_eq!(m.field, "pin");
        assert_eq!(m.rhs, MembershipRhs::Binding("peers".into()));
    }
}
