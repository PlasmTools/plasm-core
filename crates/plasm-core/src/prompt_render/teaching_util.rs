//! Shared teaching-table string helpers and placeholders.

pub(crate) use crate::taught_seat::{TEACHING_ID_HOLE, TEACHING_PARAM_VALUE_PLACEHOLDER};

/// Quoted Select/MultiSelect hole on query/search filters — not a first-member
/// exemplar (`T_enum_query_hole`). Signals the same quoting law as search `"<query>"`.
pub(crate) const TEACHING_SELECT_MEMBER_LITERAL: &str = "\"<member>\"";

/// First closed-enum member as a quoted teaching exemplar (Select / MultiSelect).
/// TSV-derivable from `NamedValueSchema.allowed_values` — not a task scalar.
pub(crate) fn select_enum_teach_literal(nv: &crate::NamedValueSchema) -> Option<String> {
    let raw = nv.allowed_values.as_ref()?.iter().find(|s| !s.is_empty())?;
    if raw.contains('"') || raw.contains('<') || raw.contains('>') {
        return None;
    }
    Some(format!("\"{raw}\""))
}

/// Search text hole including quotes: `e#~"<query>"`.
pub(crate) const TEACHING_SEARCH_QUERY_LITERAL: &str = "\"<query>\"";

/// Strip authoring noise like ``(constructor `v101`)`` from variant descriptions before teaching table Meaning.
pub(crate) fn strip_union_constructor_authoring_noise(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find("(constructor ") {
        let Some(close_rel) = s[start..].find(')') else {
            break;
        };
        let close = start + close_rel;
        let inner = s[start + "(constructor ".len()..close].trim();
        let noise = inner.contains('v') && inner.chars().any(|c| c.is_ascii_digit());
        if !noise {
            break;
        }
        let before = s[..start].trim_end();
        let after = s[close + 1..].trim_start();
        s = if before.is_empty() {
            after.to_string()
        } else if after.is_empty() {
            before.to_string()
        } else {
            format!("{before} {after}")
        };
    }
    s.trim().to_string()
}

/// Rewrite teaching angle-bracket / query holes to parseable stand-ins for validation only.
///
/// Emitted teaching keeps `<id>` / `<wire>` / `"<query>"`; the validator sees `$` / `"q"`.
pub(crate) fn teaching_expr_for_validation(expr: &str) -> String {
    if !expr.contains('<') {
        return expr.to_string();
    }
    // Replace quoted holes before the generic `<ident>` walk so `"<member>"`
    // becomes `$` (same stand-in as `<wire>`), not the string `"$"`.
    let s = expr
        .replace(TEACHING_SEARCH_QUERY_LITERAL, "\"q\"")
        .replace(TEACHING_SELECT_MEMBER_LITERAL, "$");
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel) = s[i + 1..].find('>') {
                let candidate = &s[i..i + 2 + rel];
                if crate::taught_seat::is_teaching_angle_hole(candidate) {
                    out.push('$');
                    i = i + 2 + rel;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_enum_teach_literal_quotes_first_member() {
        let nv = crate::NamedValueSchema {
            domain: Default::default(),
            description: String::new(),
            field_type: crate::FieldType::Select,
            value_format: None,
            allowed_values: Some(vec!["received".into(), "sent".into()]),
            array_items: None,
            currency: None,
        };
        assert_eq!(
            select_enum_teach_literal(&nv).as_deref(),
            Some("\"received\"")
        );
    }

    #[test]
    fn validation_proxy_rewrites_angle_holes() {
        assert_eq!(teaching_expr_for_validation(r#"e7(<id>)"#), "e7($)");
        assert_eq!(teaching_expr_for_validation(r#"e7~"<query>""#), r#"e7~"q""#);
        assert_eq!(
            teaching_expr_for_validation(r#"e1{status="<member>"}"#),
            "e1{status=$}"
        );
        assert_eq!(
            teaching_expr_for_validation("e1{title=<wire>}.m2(body=<wire>)"),
            "e1{title=$}.m2(body=$)"
        );
    }
}
