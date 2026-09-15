//! Top-level delimiter-aware splitting for assignments and lists.

use super::super::heredoc_surface::{heredoc_surface_step_at, HeredocSurfaceStep};
use super::labels::is_valid_program_label;

/// Top-level `=` that is a program binding, an invalid binding attempt, or pipe/where equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopLevelAssignment<'a> {
    Binding { label: &'a str, rhs: &'a str },
    InvalidLabel { label: &'a str },
}

/// Classify `lhs = rhs` without treating `| where field="x"` as a binding.
pub fn classify_top_level_assignment(line: &str) -> Option<TopLevelAssignment<'_>> {
    let (label, rhs) = split_assignment_at_top_level(line)?;
    if is_valid_program_label(label) && !matches!(label, "_" | "$" | "return") {
        return Some(TopLevelAssignment::Binding { label, rhs });
    }
    if label.contains('|') || label.contains(char::is_whitespace) {
        return None;
    }
    Some(TopLevelAssignment::InvalidLabel { label })
}

/// Split `lhs = rhs` at the first top-level `=` (respecting quotes and nesting).
///
/// Does **not** validate `lhs`; use [`validate_program_label`] after splitting when the line is
/// intended as a program binding.
pub fn split_assignment_at_top_level(line: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    let mut i = 0usize;
    while i < line.len() {
        if quote.is_none() {
            match heredoc_surface_step_at(line, i) {
                Ok(HeredocSurfaceStep::SkipTo(next)) => {
                    i = next;
                    continue;
                }
                Ok(HeredocSurfaceStep::OpenerIncomplete { .. }) => return None,
                Ok(HeredocSurfaceStep::NotAnOpener) | Err(_) => {}
            }
        }
        let c = line[i..].chars().next().expect("valid UTF-8 boundary");
        let cl = c.len_utf8();
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            '=' if quote.is_none() && depth == 0 => {
                if line[i..].starts_with("=>") {
                    i += cl;
                    continue;
                }
                let left = line[..i].trim();
                let right = line[i + 1..].trim();
                if !left.is_empty() && !right.is_empty() {
                    return Some((left, right));
                }
            }
            _ => {}
        }
        i += cl;
    }
    None
}

/// Split `label = rhs` at top-level `=` only when `label` is a valid program binding name.
#[inline]
pub fn split_assignment_for_binding(line: &str) -> Option<(&str, &str)> {
    let (l, r) = split_assignment_at_top_level(line)?;
    is_valid_program_label(l).then_some((l, r))
}

/// Split on `delimiter` at nesting depth 0, skipping quoted regions and tagged heredocs.
///
/// Used for comma-separated roots and aggregate argument lists. Unlike [`collect_program_statement_lines`],
/// this errors if a heredoc opener on one line is incomplete (hard newline required after `TAG`).
pub fn split_top_level(s: &str, delimiter: char) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<char>;
    let mut i = 0usize;
    while i < s.len() {
        let c = s[i..]
            .chars()
            .next()
            .ok_or_else(|| "invalid UTF-8 boundary".to_string())?;
        let cl = c.len_utf8();
        if quote.is_none() {
            match heredoc_surface_step_at(s, i)? {
                HeredocSurfaceStep::NotAnOpener => {}
                HeredocSurfaceStep::OpenerIncomplete { .. } => {
                    return Err(
                        "tagged heredoc `<<TAG` must have a newline immediately after the tag on the opener line (hard newline; do not squash `<<TAG` with the body on one line)".into(),
                    );
                }
                HeredocSurfaceStep::SkipTo(next) => {
                    i = next;
                    continue;
                }
            }
        }
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            _ if c == delimiter && quote.is_none() && depth == 0 => {
                out.push(&s[start..i]);
                start = i + cl;
            }
            _ => {}
        }
        i += cl;
    }
    if depth != 0 {
        return Err(format!("unbalanced delimiters in `{s}`"));
    }
    out.push(&s[start..]);
    Ok(out)
}

/// Split at the first top-level occurrence of `token` (e.g. `"=>"` for effect templates).
pub fn split_token_top_level<'a>(
    line: &'a str,
    token: &str,
) -> Result<Option<(&'a str, &'a str)>, String> {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    let bytes = line.as_bytes();
    let token_b = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = line[i..].chars().next().ok_or("invalid UTF-8 boundary")?;
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            _ => {}
        }
        if quote.is_none() && depth == 0 && bytes[i..].starts_with(token_b) {
            return Ok(Some((&line[..i], &line[i + token.len()..])));
        }
        i += c.len_utf8();
    }
    Ok(None)
}
