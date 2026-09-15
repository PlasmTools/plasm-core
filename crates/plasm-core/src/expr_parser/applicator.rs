//! Application stratum (`=>` applicators): derive, render, row application, relation fanout.

use super::heredoc_surface::tagged_heredoc_close_kind;
use super::{split_top_level, validate_program_label};

/// Right-hand side of `rowset => applicator` (stratum 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applicator {
    /// `=> { k: _.f, … }`
    Derive { body: String },
    /// `=> <<TAG … TAG`
    Render { kind: RenderApplicator },
    /// A catalog read or operation evaluated once per source row.
    ///
    /// Examples: `=> Entity(_.id)`, `=> Entity{parent_id=_.id}` and
    /// `=> Entity.m#(…, _.f)`.
    Apply { surface: String },
    /// `=> _.r#` / `=> _.wire`
    Relation { wire: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderApplicator {
    /// Left side is the staged rowset source.
    Inferred { template: String },
    /// Left side lists in-scope binding labels (`a,b,c`).
    CrossBinding {
        sources: Vec<String>,
        template: String,
    },
}

/// Parse the RHS of a top-level `=>` (already split; no leading `=>`).
pub fn parse_applicator(raw: &str) -> Result<Applicator, String> {
    let right = raw.trim();
    if right.is_empty() {
        return Err(
            "`=>` requires an applicator (`{ … }`, `<<TAG`, `Entity.m#(…)`, or `_.r#`)".into(),
        );
    }
    if let Some(opener) = right.strip_prefix("<<") {
        let template = parse_render_template_after_tag_opener(opener)?;
        return Ok(Applicator::Render {
            kind: RenderApplicator::Inferred { template },
        });
    }
    if let Some(wire) = right.strip_prefix("_.") {
        if method_call_at_depth_zero(right) {
            return Ok(Applicator::Apply {
                surface: right.to_string(),
            });
        }
        let wire = wire.trim();
        if wire.is_empty() || wire.contains(char::is_whitespace) || wire.contains('(') {
            return Err(format!(
                "relation applicator must be `_.r#` or `_.wire` (got `=> {right}`)"
            ));
        }
        return Ok(Applicator::Relation {
            wire: wire.to_string(),
        });
    }
    if right.starts_with('{') {
        return Ok(Applicator::Derive {
            body: right.to_string(),
        });
    }
    if is_row_application_surface(right) {
        return Ok(Applicator::Apply {
            surface: right.to_string(),
        });
    }
    Err(format!(
        "unsupported `=>` applicator `{right}`; use a row-producing form such as `=> {{ … }}`, `=> <<TAG`, `=> Entity(_.id)`, `=> Entity{{parent_id=_.id}}`, `=> Entity.m#(…, _)`, or `=> _.r#`"
    ))
}

fn parse_render_template_after_tag_opener(rest: &str) -> Result<String, String> {
    let bytes = rest.as_bytes();
    if rest.is_empty() {
        return Err(
            "row-to-text template heredoc: expected `<<TAG` then newline after the tag on the opener line"
                .into(),
        );
    }
    let b0 = bytes[0];
    if !(b0.is_ascii_alphabetic() || b0 == b'_') {
        return Err(
            "row-to-text template heredoc: `<<` must be followed by `TAG` ([A-Za-z_][A-Za-z0-9_]*) then newline"
                .into(),
        );
    }
    let mut pos = 1usize;
    while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
        pos += 1;
    }
    let tag = &rest[..pos];
    if pos >= bytes.len() || bytes[pos] != b'\n' {
        return Err(format!(
            "row-to-text template heredoc `<<{tag}`: newline required immediately after the tag on the opener line (same rule as structured parameter heredocs)"
        ));
    }
    pos += 1;
    let body_start = pos;
    loop {
        let line_start = pos;
        while pos < bytes.len() && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
            pos += 1;
        }
        let line_slice = &rest[line_start..pos];
        if tagged_heredoc_close_kind(line_slice, tag).is_some() {
            return Ok(rest[body_start..line_start].to_string());
        }
        if pos >= bytes.len() {
            return Err(format!(
                "row-to-text template heredoc `<<{tag}` is not closed: after the template body, add a line whose trimmed text is `{tag}` (closes on the first such line)"
            ));
        }
        if bytes[pos] == b'\r' {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        } else {
            return Err(format!(
                "row-to-text template heredoc `<<{tag}`: malformed line terminator inside template body"
            ));
        }
    }
}

fn try_parse_cross_binding_sources(head: &str) -> Option<Vec<String>> {
    if !head.contains(',') {
        return None;
    }
    let parts = split_top_level(head, ',').ok()?;
    if parts.len() < 2 {
        return None;
    }
    let mut sources = Vec::with_capacity(parts.len());
    for part in parts {
        let label = part.trim();
        if label.is_empty()
            || label.contains('.')
            || label.contains('(')
            || label.contains('[')
            || label.contains('~')
        {
            return None;
        }
        validate_program_label(label).ok()?;
        sources.push(label.to_string());
    }
    Some(sources)
}

/// True when `rhs` is a catalog source or operation evaluated in row scope.
fn is_row_application_surface(rhs: &str) -> bool {
    let t = rhs.trim();
    if t.starts_with('{') || t.starts_with("<<") || t.starts_with("_.") {
        return false;
    }
    if method_call_at_depth_zero(t) {
        return true;
    }

    // Catalog Get / Query / Search. Context-sensitive entity resolution and
    // row-reference type checking belong to typed elaboration, not this lexer.
    let Some(first) = t.as_bytes().first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || *first == b'_') {
        return false;
    }
    let head_end = t
        .char_indices()
        .find_map(|(idx, ch)| (!(ch.is_ascii_alphanumeric() || ch == '_')).then_some(idx))
        .unwrap_or(t.len());
    matches!(t.as_bytes().get(head_end), Some(b'(' | b'{' | b'~'))
}

/// `.method(` at paren/bracket/brace depth 0 outside quotes.
/// Method names: leading alpha/`_`, then alnum / `_` / `-` (covers `m12` and `secured-touch`).
pub(crate) fn method_call_at_depth_zero(t: &str) -> bool {
    let bytes = t.as_bytes();
    let mut i = 0;
    let mut depth = 0i32;
    let mut quote = None::<u8>;
    while i + 1 < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' if quote == Some(b) => quote = None,
            b'"' | b'\'' if quote.is_none() => quote = Some(b),
            b'(' | b'[' | b'{' if quote.is_none() => depth += 1,
            b')' | b']' | b'}' if quote.is_none() => depth -= 1,
            b'.' if quote.is_none() && depth == 0 => {
                let mut j = i + 1;
                if j >= bytes.len() || !(bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                    i += 1;
                    continue;
                }
                j += 1;
                while j < bytes.len()
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-')
                {
                    j += 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Split `pipe_or_primary => applicator` at top level; parse applicator when present.
pub fn split_apply_expr(raw: &str) -> Result<(String, Option<Applicator>), String> {
    match super::split_token_top_level(raw, "=>")? {
        None => Ok((raw.trim().to_string(), None)),
        Some((left, right)) => {
            let left = left.trim().to_string();
            if left.is_empty() {
                return Err("`=>` requires a left-hand rowset".into());
            }
            let mut app = parse_applicator(right)?;
            if let Applicator::Render {
                kind: RenderApplicator::Inferred { template },
            } = app
            {
                app = if let Some(sources) = try_parse_cross_binding_sources(&left) {
                    Applicator::Render {
                        kind: RenderApplicator::CrossBinding { sources, template },
                    }
                } else {
                    Applicator::Render {
                        kind: RenderApplicator::Inferred { template },
                    }
                };
            }
            Ok((left, Some(app)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_not_stolen_by_message_field() {
        let a = parse_applicator("{ t: _.message, o: _.owner }").unwrap();
        assert!(matches!(a, Applicator::Derive { .. }));
    }

    #[test]
    fn foreach_opaque_m_digit() {
        let a = parse_applicator("LangItem.m3(title=_.title)").unwrap();
        assert!(matches!(a, Applicator::Apply { .. }));
    }

    #[test]
    fn foreach_wire_update_and_secured_touch() {
        let a = parse_applicator("LangItem(_.id).update(score=9, title=_.title)").unwrap();
        assert!(matches!(a, Applicator::Apply { .. }));
        let b = parse_applicator("LangItem(_.id).secured-touch(access_token=auth.access_token)")
            .unwrap();
        assert!(matches!(b, Applicator::Apply { .. }));

        let get = parse_applicator("LangItem(_.id)").unwrap();
        assert!(matches!(get, Applicator::Apply { .. }));
        let query = parse_applicator("LangItem{owner_id=_.id}").unwrap();
        assert!(matches!(query, Applicator::Apply { .. }));
    }

    #[test]
    fn foreach_rejects_bare_message_token_as_effect() {
        // `.message` alone is not an effect surface — must be derive or error.
        let err = parse_applicator("row.message").unwrap_err();
        assert!(err.contains("unsupported"), "{err}");
    }

    #[test]
    fn relation_wire() {
        let a = parse_applicator("_.tags").unwrap();
        assert_eq!(
            a,
            Applicator::Relation {
                wire: "tags".into()
            }
        );
    }

    #[test]
    fn parse_applicator_parses_render_rhs() {
        let app = parse_applicator("<<MD\nhello\nMD").unwrap();
        assert!(matches!(
            app,
            Applicator::Render {
                kind: RenderApplicator::Inferred { ref template }
            } if template == "hello\n"
        ));
    }

    #[test]
    fn split_apply_parses_inferred_render() {
        let (_left, app) = split_apply_expr("commits => <<MD\n{{ rows }}\nMD").unwrap();
        assert!(matches!(
            app,
            Some(Applicator::Render {
                kind: RenderApplicator::Inferred { ref template }
            }) if template.contains("rows")
        ));
    }

    #[test]
    fn split_apply_parses_cross_binding_render() {
        let (_left, app) = split_apply_expr("a,b => <<MD\n{{ a }} {{ b }}\nMD").unwrap();
        assert!(matches!(
            app,
            Some(Applicator::Render {
                kind: RenderApplicator::CrossBinding { ref sources, .. }
            }) if sources == &vec!["a".to_string(), "b".to_string()]
        ));
    }
}
