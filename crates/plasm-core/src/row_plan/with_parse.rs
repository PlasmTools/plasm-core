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
    for part in split_top_level_comma(body)? {
        let part = part.trim();
        let Some((name, expr)) = part.split_once(':') else {
            return Err(WithExprError::Parse(format!(
                "expected `name: expr`, got `{part}`"
            )));
        };
        let name = OutputName::new(name.trim().to_string()).map_err(WithExprError::BadColumn)?;
        let expr = parse_with_expr(expr.trim())?;
        columns.push(WithColumn { name, expr });
    }
    if columns.is_empty() {
        return Err(WithExprError::EmptyBody);
    }
    Ok(columns)
}

/// Lexical structure shared by separators and arithmetic: quoted text is opaque.
fn top_level_chars(s: &str) -> Result<Vec<(usize, char)>, WithExprError> {
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut top = Vec::new();
    for (i, c) in s.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' => stack.push(')'),
            ')' => {
                if stack.pop() != Some(')') {
                    return Err(WithExprError::Parse(
                        "unbalanced expression parentheses".into(),
                    ));
                }
            }
            _ if stack.is_empty() => top.push((i, c)),
            _ => {}
        }
    }
    if quoted || !stack.is_empty() {
        return Err(WithExprError::Parse(
            "unterminated string or parenthesized expression".into(),
        ));
    }
    Ok(top)
}

fn split_top_level_comma(s: &str) -> Result<Vec<&str>, WithExprError> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in top_level_chars(s)? {
        if c == ',' {
            out.push(&s[start..i]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    Ok(out)
}

fn parse_with_expr(s: &str) -> Result<WithExpr, WithExprError> {
    let s = s.trim();
    parse_arith(s)
}

fn parse_arith(s: &str) -> Result<WithExpr, WithExprError> {
    top_level_chars(s)?;
    if let Ok(i) = s.parse::<i64>() {
        return Ok(WithExpr::Literal(WithLiteral::Integer(i)));
    }
    if let Ok(n) = s.parse::<f64>() {
        if !n.is_finite() {
            return Err(WithExprError::Parse("number must be finite".into()));
        }
        return Ok(WithExpr::Literal(WithLiteral::Number(s.to_owned())));
    }
    if let Some((op, lhs, rhs)) = split_top_addsub(s) {
        return Ok(WithExpr::Arith {
            op,
            lhs: Box::new(parse_arith(lhs)?),
            rhs: Box::new(parse_arith(rhs)?),
        });
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

fn split_top_addsub(s: &str) -> Option<(ArithOp, &str, &str)> {
    top_level_chars(s)
        .ok()?
        .into_iter()
        .rev()
        .find_map(|(i, c)| {
            let op = match c {
                '+' => ArithOp::Add,
                '-' => ArithOp::Sub,
                _ => return None,
            };
            let lhs = s[..i].trim();
            if lhs.is_empty() || lhs.ends_with(['+', '-', '*', '/']) {
                return None;
            }
            // Signs within a scientific literal are not binary operators.
            if lhs.ends_with(['e', 'E']) {
                let token = lhs.rsplit(['+', '-', '*', '/', '(', ' ', ',']).next()?;
                if token[..token.len() - 1].parse::<f64>().is_ok() {
                    return None;
                }
            }
            Some((op, lhs, s[i + 1..].trim()))
        })
}

fn split_top_muldiv(s: &str) -> Option<(ArithOp, &str, &str)> {
    top_level_chars(s)
        .ok()?
        .into_iter()
        .rev()
        .find_map(|(i, c)| {
            if i == 0 {
                return None;
            }
            let op = match c {
                '*' => ArithOp::Mul,
                '/' => ArithOp::Div,
                _ => return None,
            };
            Some((op, s[..i].trim(), s[i + 1..].trim()))
        })
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
    if s.starts_with('"') {
        let value = serde_json::from_str::<String>(s)
            .map_err(|e| WithExprError::Parse(format!("invalid string literal: {e}")))?;
        return Ok(WithExpr::Literal(WithLiteral::String(value)));
    }
    if let Some(rest) = s.strip_prefix("len(").and_then(|t| t.strip_suffix(')')) {
        return Ok(WithExpr::Len {
            field: surface_field_path(rest.trim()).map_err(WithExprError::Parse)?,
        });
    }
    if let Some(rest) = s.strip_prefix("when(").and_then(|t| t.strip_suffix(')')) {
        return parse_when(rest);
    }
    if s.contains('|') {
        return Err(WithExprError::Parse("row expressions do not accept template filter pipes; use Minijinja inside {{ }} in a string or per-row => <<TAG template".into()));
    }
    if let Some(idx) = s.find('(') {
        if s.ends_with(')') {
            let fname = &s[..idx];
            return Err(WithExprError::Parse(format!(
                "unknown .with function `{fname}` (known calls: len, when; `now` is a word, not a call)"
            )));
        }
    }
    surface_field_path(s)
        .map(WithExpr::Field)
        .map_err(WithExprError::Parse)
}

/// Outer `(`…`)` only when that pair wraps the whole atom (`(now - t)`, not `(a)+(b)`).
fn strip_wrapping_parens(s: &str) -> Option<&str> {
    if !s.starts_with('(') || !s.ends_with(')') {
        return None;
    }
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (i + 1 == s.len()).then(|| s[1..i].trim());
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_when(args: &str) -> Result<WithExpr, WithExprError> {
    let parts = split_top_level_comma(args)?;
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
    for (i, _) in top_level_chars(s)? {
        if i == 0 {
            continue;
        }
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
    Err(WithExprError::Parse(format!(
        "when() predicate must be a comparison, got `{s}`"
    )))
}

fn surface_field_path(s: &str) -> Result<FieldPath, String> {
    if !s.split('.').all(|part| {
        let mut chars = part.chars();
        chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && chars.all(|c| c == '_' || c.is_alphanumeric())
    }) {
        return Err(format!(
            "invalid computed field reference `{s}`; use a row field or a supported expression"
        ));
    }
    FieldPath::from_dotted(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computed_expression_requires_complete_valid_syntax() {
        for input in [
            "out: field garbage",
            "out: unknown(field)",
            "out: (field",
            "out: field)",
            "out: \"unterminated",
            "out: len(field + 1)",
            "out: field | last",
            "out: 1e999",
        ] {
            assert!(parse_with_body(input).is_err(), "accepted {input}");
        }
        for input in [
            "out: 1e-3 + score",
            "out: score * -2",
            "out: when(title = \"a=b\", \"(,|)\", \"x\")",
            "out: (score - 2) + 3",
        ] {
            assert!(parse_with_body(input).is_ok(), "rejected {input}");
        }
    }
}
