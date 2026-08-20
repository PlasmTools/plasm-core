//! Parse `.with{name: expr, …}` bodies.

use crate::plasm_monad::payload::{FieldPath, PlanPredicateOp};
use crate::plasm_monad::{OutputName, WithColumn, WithExpr, WithExprError, WithLiteral};

use super::expr::ArithOp;

pub fn parse_with_body(body: &str) -> Result<Vec<WithColumn>, WithExprError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(WithExprError::EmptyBody);
    }
    let mut columns = Vec::new();
    for part in split_top_level_comma(body) {
        let part = part.trim();
        let Some((name, expr)) = part.split_once(':') else {
            return Err(WithExprError::Parse(format!(
                "expected `name: expr`, got `{part}`"
            )));
        };
        let name =
            OutputName::new(name.trim().to_string()).map_err(WithExprError::BadColumn)?;
        let expr = parse_with_expr(expr.trim())?;
        columns.push(WithColumn { name, expr });
    }
    if columns.is_empty() {
        return Err(WithExprError::EmptyBody);
    }
    Ok(columns)
}

fn split_top_level_comma(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_with_expr(s: &str) -> Result<WithExpr, WithExprError> {
    let s = s.trim();
    parse_arith(s)
}

fn parse_arith(s: &str) -> Result<WithExpr, WithExprError> {
    if let Some((lhs, rhs)) = split_top_bin(s, '+') {
        return Ok(WithExpr::Arith {
            op: ArithOp::Add,
            lhs: Box::new(parse_arith(lhs)?),
            rhs: Box::new(parse_arith(rhs)?),
        });
    }
    if let Some((lhs, rhs)) = split_top_bin(s, '-') {
        if !lhs.trim().is_empty() {
            return Ok(WithExpr::Arith {
                op: ArithOp::Sub,
                lhs: Box::new(parse_arith(lhs)?),
                rhs: Box::new(parse_arith(rhs)?),
            });
        }
    }
    if let Some((op, lhs, rhs)) = split_top_muldiv(s) {
        return Ok(WithExpr::Arith {
            op,
            lhs: Box::new(parse_arith(lhs)?),
            rhs: Box::new(parse_arith(rhs)?),
        });
    }
    parse_atom(s)
}

fn split_top_bin(s: &str, op: char) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices().rev() {
        match c {
            ')' | '}' => depth += 1,
            '(' | '{' => depth -= 1,
            c if c == op && depth == 0 && i > 0 => {
                return Some((s[..i].trim(), s[i + op.len_utf8()..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn split_top_muldiv(s: &str) -> Option<(ArithOp, &str, &str)> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices().rev() {
        match c {
            ')' | '}' => depth += 1,
            '(' | '{' => depth -= 1,
            '*' | '/' if depth == 0 && i > 0 => {
                let op = if c == '*' { ArithOp::Mul } else { ArithOp::Div };
                return Some((op, s[..i].trim(), s[i + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn parse_atom(s: &str) -> Result<WithExpr, WithExprError> {
    let s = s.trim();
    if let Some(inner) = strip_wrapping_parens(s) {
        return parse_with_expr(inner);
    }
    if s.eq_ignore_ascii_case("null") {
        return Ok(WithExpr::Literal(WithLiteral::Null));
    }
    if s.eq_ignore_ascii_case("true") {
        return Ok(WithExpr::Literal(WithLiteral::Bool(true)));
    }
    if s.eq_ignore_ascii_case("false") {
        return Ok(WithExpr::Literal(WithLiteral::Bool(false)));
    }
    if s.eq_ignore_ascii_case("now") {
        return Ok(WithExpr::Now);
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return Ok(WithExpr::Literal(WithLiteral::String(inner.to_string())));
    }
    if let Some(rest) = s.strip_prefix("len(").and_then(|t| t.strip_suffix(')')) {
        return Ok(WithExpr::Len {
            field: FieldPath::from_dotted(rest.trim()).map_err(WithExprError::Parse)?,
        });
    }
    if let Some(rest) = s.strip_prefix("when(").and_then(|t| t.strip_suffix(')')) {
        return parse_when(rest);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Ok(WithExpr::Literal(WithLiteral::Integer(i)));
    }
    if s.parse::<f64>().is_ok() {
        return Ok(WithExpr::Literal(WithLiteral::Number(s.to_string())));
    }
    if let Some(idx) = s.find('(') {
        if s.ends_with(')') {
            let fname = &s[..idx];
            return Err(WithExprError::Parse(format!(
                "unknown .with function `{fname}` (known calls: len, when; `now` is a word, not a call)"
            )));
        }
    }
    FieldPath::from_dotted(s)
        .map(WithExpr::Field)
        .map_err(WithExprError::Parse)
}

/// Outer `(`…`)` only when that pair wraps the whole atom (`(now - t)`, not `(a)+(b)`).
fn strip_wrapping_parens(s: &str) -> Option<&str> {
    if !s.starts_with('(') || !s.ends_with(')') {
        return None;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    if i + 1 == s.len() {
                        return Some(s[1..i].trim());
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_when(args: &str) -> Result<WithExpr, WithExprError> {
    let parts = split_top_level_comma(args);
    if parts.len() != 3 {
        return Err(WithExprError::Parse(
            "when(pred, then, else) requires three arguments".into(),
        ));
    }
    let (lhs, op, rhs) = split_when_cmp(parts[0].trim())?;
    Ok(WithExpr::When {
        lhs: Box::new(parse_with_expr(lhs)?),
        op,
        rhs: Box::new(parse_with_expr(rhs)?),
        then: Box::new(parse_with_expr(parts[1])?),
        else_: Box::new(parse_with_expr(parts[2])?),
    })
}

fn split_when_cmp(s: &str) -> Result<(&str, PlanPredicateOp, &str), WithExprError> {
    let ops: [(&str, PlanPredicateOp); 6] = [
        (">=", PlanPredicateOp::Gte),
        ("<=", PlanPredicateOp::Lte),
        ("!=", PlanPredicateOp::Ne),
        ("=", PlanPredicateOp::Eq),
        (">", PlanPredicateOp::Gt),
        ("<", PlanPredicateOp::Lt),
    ];
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            _ if depth == 0 && i > 0 => {
                for (sym, op) in ops {
                    if s[i..].starts_with(sym) {
                        let lhs = s[..i].trim();
                        let rhs = s[i + sym.len()..].trim();
                        if !lhs.is_empty() && !rhs.is_empty() {
                            return Ok((lhs, op, rhs));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Err(WithExprError::Parse(format!(
        "when() predicate must be a comparison, got `{s}`"
    )))
}
