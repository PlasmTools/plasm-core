//! Shared teaching-table string helpers and placeholders.

/// Identity-get hole in teaching exemplars: `e#(<id>)` / compound `wire=<id>`.
pub(crate) const TEACHING_ID_HOLE: &str = "<id>";

/// Generic capability / filter param hole: `wire=<wire>` (never bare `$`).
pub(crate) const TEACHING_PARAM_VALUE_PLACEHOLDER: &str = "<wire>";

/// Search text hole including quotes: `e#~"<query>"`.
pub(crate) const TEACHING_SEARCH_QUERY_LITERAL: &str = "\"<query>\"";

pub(crate) fn truncate_inline_desc(s: &str, max: usize) -> String {
    let t = crate::symbol_tuning::trim_description_for_agent_gloss(s).replace('\t', " ");
    crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(&t, max)
}

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
    let s = expr.replace(TEACHING_SEARCH_QUERY_LITERAL, "\"q\"");
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel) = s[i + 1..].find('>') {
                let inner = &s[i + 1..i + 1 + rel];
                if !inner.is_empty()
                    && inner
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
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
    fn validation_proxy_rewrites_angle_holes() {
        assert_eq!(
            teaching_expr_for_validation(r#"e7(<id>)"#),
            "e7($)"
        );
        assert_eq!(
            teaching_expr_for_validation(r#"e7~"<query>""#),
            r#"e7~"q""#
        );
        assert_eq!(
            teaching_expr_for_validation("e1{title=<wire>}.m2(body=<wire>)"),
            "e1{title=$}.m2(body=$)"
        );
    }
}
