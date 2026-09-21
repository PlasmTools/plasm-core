//! Untaught row-filter repair syntax. No catalog selection or authorization changes.
use crate::{expr_parser::split_top_level, BooleanExpr};

pub fn parse_boolean_filter(body: &str) -> Result<BooleanExpr<String>, String> {
    parse(body.trim(), 0)
}

fn parse(body: &str, depth: usize) -> Result<BooleanExpr<String>, String> {
    if depth > 64 {
        return Err("row filter nesting exceeds 64 levels".into());
    }
    if body.is_empty() {
        return Err("empty predicate: supply a field comparison after `where`".into());
    }
    for (keyword, conjunction) in [("or", false), ("and", true)] {
        let pieces = split_boolean(body, keyword, conjunction)?;
        if pieces.len() > 1 {
            let args = pieces
                .into_iter()
                .map(|p| parse(p.trim(), depth + 1))
                .collect::<Result<_, _>>()?;
            return Ok(if conjunction {
                BooleanExpr::And(args)
            } else {
                BooleanExpr::Or(args)
            });
        }
    }
    if body.len() > 3
        && body.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("not"))
        && body.as_bytes()[3].is_ascii_whitespace()
    {
        return Ok(BooleanExpr::Not(Box::new(parse(
            body[3..].trim(),
            depth + 1,
        )?)));
    }
    if body.starts_with('(') && body.ends_with(')') {
        return parse(&body[1..body.len() - 1], depth + 1);
    }
    if let Some((field, anti, rhs)) = crate::row_membership::split_membership_op(body)? {
        let rhs = rhs.trim();
        if rhs.starts_with('(') && rhs.ends_with(')') {
            let inner = &rhs[1..rhs.len() - 1];
            // A pipeline remains a rowset membership expression; literals are compared
            // individually through the existing field-directed scalar coercion path.
            if split_top_level(inner, '|')?.len() == 1 {
                let values = split_top_level(inner, ',')?;
                if values.is_empty() || values.iter().any(|v| v.trim().is_empty()) {
                    return Err("literal membership requires a non-empty scalar list".into());
                }
                let mut args = Vec::new();
                for value in values {
                    let value = value.trim();
                    let scalar = serde_json::from_str::<serde_json::Value>(value)
                        .map_err(|_| "membership list entries must be JSON scalar literals; use a one-column rowset for computed values".to_string())?;
                    if scalar.is_array() || scalar.is_object() || scalar.is_null() {
                        return Err(
                            "membership list entries must be non-null scalar literals".into()
                        );
                    }
                    args.push(BooleanExpr::Atom(format!(
                        "{field} {} {value}",
                        if anti { "!=" } else { "=" }
                    )));
                }
                return Ok(if anti {
                    BooleanExpr::And(args)
                } else {
                    BooleanExpr::Or(args)
                });
            }
        }
    }
    Ok(BooleanExpr::Atom(body.to_string()))
}

fn split_boolean<'a>(body: &'a str, keyword: &str, comma: bool) -> Result<Vec<&'a str>, String> {
    let mut quote = None;
    let mut escaped = false;
    let mut stack = Vec::new();
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote.is_some() {
            if c == '\\' {
                escaped = true;
            } else if Some(c) == quote {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            continue;
        }
        match c {
            '(' | '[' | '{' => {
                stack.push(c);
                continue;
            }
            ')' | ']' | '}' => {
                let expected = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.pop() != Some(expected) {
                    return Err("unbalanced row-filter delimiters".into());
                }
                continue;
            }
            _ => {}
        }
        if !stack.is_empty() || i < start {
            continue;
        }
        let is_keyword = body
            .get(i..i + keyword.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(keyword))
            && (i == 0
                || body[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || c == ')'))
            && body.get(i + keyword.len()..).is_some_and(|s| {
                s.is_empty() || s.starts_with(char::is_whitespace) || s.starts_with('(')
            });
        if (comma && c == ',') || is_keyword {
            out.push(&body[start..i]);
            start = i + if is_keyword { keyword.len() } else { 1 };
        }
    }
    if quote.is_some() || !stack.is_empty() {
        return Err("unclosed quote or delimiter in row filter".into());
    }
    out.push(&body[start..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_membership_and_quoted_keywords() {
        let tree = parse_boolean_filter(r#"x = 1 OR y in (2, 3) AND z = "or and , ()""#).unwrap();
        let BooleanExpr::Or(parts) = tree else {
            panic!("OR must be outermost")
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[1], BooleanExpr::And(_)));
        let tree = parse_boolean_filter(r#"NOT (x not in (1, 2) or x = 3)"#).unwrap();
        assert!(matches!(tree, BooleanExpr::Not(_)));
    }
    #[test]
    fn malformed_membership_is_actionable() {
        for s in [
            "x in ()",
            "x in (1,)",
            "x in (null)",
            "x in ({})",
            "x in (foo)",
        ] {
            let err = parse_boolean_filter(s).unwrap_err();
            assert!(err.contains("membership"), "{s}: {err}");
        }
        assert!(parse_boolean_filter("x = 1 AND")
            .unwrap_err()
            .contains("empty predicate"));
        assert!(parse_boolean_filter("(x = 1").is_err());
    }
    proptest::proptest! {
        #[test]
        fn sugar_roundtrip_preserves_boolean_selection(values in proptest::collection::vec(-20i32..20, 1..20), x in -25i32..25, anti in proptest::bool::ANY) {
            let op = if anti { "not in" } else { "in" };
            let input = format!("x {op} ({})", values.iter().map(ToString::to_string).collect::<Vec<_>>().join(","));
            let tree = parse_boolean_filter(&input).unwrap();
            let rendered = tree.render(&Clone::clone);
            let parsed = parse_boolean_filter(&rendered).unwrap();
            let wire = serde_json::to_vec(&parsed).unwrap();
            let restored: BooleanExpr<String> = serde_json::from_slice(&wire).unwrap();
            let got = restored.evaluate(&|atom| {
                let n = atom.split_whitespace().last().unwrap().parse::<i32>().unwrap();
                Some(if anti { x != n } else { x == n })
            });
            proptest::prop_assert_eq!(got, Some(values.contains(&x) != anti));
        }
    }
}
