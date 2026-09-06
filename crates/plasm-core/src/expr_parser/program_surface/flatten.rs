//! Flattened one-line program expansion (repair sugar).

use super::labels::is_valid_program_label;
use super::physical_lines::strip_line_comment;
use super::split::{split_assignment_at_top_level, split_assignment_for_binding};

fn starts_like_statement_or_root(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if matches!(first, 'e' | 'p' | 'm' | 'r') {
        let mut chars = s.chars();
        chars.next();
        if matches!(chars.next(), Some(c) if c.is_ascii_digit()) {
            return true;
        }
    }
    let token = s
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | '[' | '{' | '.' | '='))
        .next()
        .unwrap_or_default();
    is_valid_program_label(token)
}

/// PLP-8 `iterate … until field = value take N` owns depth-0 `=` / trailing tokens; flatten
/// repair must not carve `until` predicates into phantom `label = …` bindings.
fn is_state_iterate_rhs(rhs: &str) -> bool {
    let t = rhs.trim_start();
    t.starts_with("iterate")
        && (t.len() == "iterate".len()
            || t.as_bytes()
                .get("iterate".len())
                .is_some_and(|b| b.is_ascii_whitespace()))
}

fn find_flattened_assignment_split(rhs: &str) -> Option<usize> {
    if is_state_iterate_rhs(rhs) {
        return None;
    }
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (i, c) in rhs.char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            '=' if quote.is_none() && depth == 0 => {
                let before_eq = &rhs[..i];
                if before_eq.ends_with('>') {
                    continue;
                }
                let before_trimmed = before_eq.trim_end();
                let token_start = before_trimmed
                    .char_indices()
                    .rev()
                    .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx + ch.len_utf8()))
                    .unwrap_or(0);
                let label = &before_trimmed[token_start..];
                if token_start > 0 && is_valid_program_label(label) {
                    return Some(token_start);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_flattened_root_split(rhs: &str) -> Option<usize> {
    if is_state_iterate_rhs(rhs) {
        return None;
    }
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (i, c) in rhs.char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            c if quote.is_none() && depth == 0 && c.is_whitespace() => {
                let left = rhs[..i].trim();
                let right = rhs[i..].trim();
                if left.ends_with("=>") || right.starts_with("=>") {
                    continue;
                }
                if !left.is_empty() && starts_like_statement_or_root(right) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn line_has_flattened_program_shape(line: &str) -> bool {
    let Some((_label, rhs)) = split_assignment_at_top_level(line) else {
        return false;
    };
    find_flattened_assignment_split(rhs).is_some() || find_flattened_root_split(rhs).is_some()
}

/// One physical line after optional flatten coercion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlattenedProgramLine {
    pub statements: Vec<String>,
    /// Set when the trailing root was rewritten to the first binding label.
    pub coerced_default_return: Option<String>,
}

/// Logical program statements after flatten expansion across physical lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlattenedProgram {
    pub statements: Vec<String>,
    /// Last non-empty coercion applied while expanding (if any).
    pub coerced_default_return: Option<String>,
}

impl FlattenedProgram {
    #[inline]
    pub fn statement_lines(&self) -> &[String] {
        &self.statements
    }
}

/// Split one physical line that contains space-separated bindings / trailing roots into logical statements.
pub fn split_flattened_program_line(line: &str) -> FlattenedProgramLine {
    let line = strip_line_comment(line).trim();
    if line.is_empty()
        || line.contains("<<")
        || line.contains('|')
        || !line_has_flattened_program_shape(line)
    {
        return FlattenedProgramLine {
            statements: vec![line.to_string()],
            coerced_default_return: None,
        };
    }
    let Some((first_label, _)) = split_assignment_at_top_level(line) else {
        return FlattenedProgramLine {
            statements: vec![line.to_string()],
            coerced_default_return: None,
        };
    };
    if !is_valid_program_label(first_label) {
        return FlattenedProgramLine {
            statements: vec![line.to_string()],
            coerced_default_return: None,
        };
    }

    let mut parts: Vec<String> = Vec::new();
    let mut rest = line;
    loop {
        let Some((label, rhs)) = split_assignment_at_top_level(rest) else {
            let tail = rest.trim();
            if !tail.is_empty() {
                parts.push(tail.to_string());
            }
            break;
        };
        if !is_valid_program_label(label) {
            parts.push(rest.trim().to_string());
            break;
        }
        if let Some(at) = find_flattened_assignment_split(rhs) {
            let binding_rhs = rhs[..at].trim();
            parts.push(format!("{label} = {binding_rhs}"));
            rest = rhs[at..].trim();
            continue;
        }
        if let Some(at) = find_flattened_root_split(rhs) {
            let binding_rhs = rhs[..at].trim();
            parts.push(format!("{label} = {binding_rhs}"));
            rest = rhs[at..].trim();
            continue;
        }
        parts.push(format!("{label} = {}", rhs.trim()));
        break;
    }
    let coerced_default_return = finalize_flattened_line_roots(&mut parts);
    FlattenedProgramLine {
        statements: parts,
        coerced_default_return,
    }
}

pub(super) fn leading_identifier(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// A trailing flat-line root is a deliberate return when it applies a postfix/projection to an
/// in-scope binding (e.g. `comments[p2,p14]`, `comments.limit(5)[p2,p14]`) — not a bare side-label
/// echo and not a fresh-entity expression.
fn trailing_root_returns_in_scope_binding(last: &str, prior: &[String]) -> bool {
    let last = last.trim();
    let head = leading_identifier(last);
    if head.is_empty() || head.len() == last.len() {
        return false;
    }
    if !is_valid_program_label(head) {
        return false;
    }
    prior.iter().any(|p| {
        split_assignment_for_binding(p)
            .map(|(label, _)| label == head)
            .unwrap_or(false)
    })
}

/// Flat single-line sugar: append or replace trailing root within space-split `parts` only.
fn finalize_flattened_line_roots(parts: &mut Vec<String>) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let first_label =
        split_assignment_at_top_level(&parts[0]).map(|(label, _)| label.to_string())?;
    let last_idx = parts.len() - 1;
    let last = parts[last_idx].trim().to_string();
    if split_assignment_for_binding(&last).is_some() {
        parts.push(first_label.clone());
        return Some(first_label);
    }
    if last == first_label {
        return None;
    }
    if trailing_root_returns_in_scope_binding(&last, &parts[..last_idx]) {
        return None;
    }
    parts[last_idx] = first_label.clone();
    Some(first_label)
}

/// Binding-only omission: append last binding when no return line exists (Tier 3).
fn coerce_binding_only_program_roots(statements: &mut Vec<String>) -> Option<String> {
    if statements.is_empty() {
        return None;
    }
    if !statements
        .iter()
        .all(|s| split_assignment_for_binding(s).is_some())
    {
        return None;
    }
    let last_idx = statements.len() - 1;
    let last_label =
        split_assignment_at_top_level(&statements[last_idx]).map(|(label, _)| label.to_string())?;
    statements.push(last_label.clone());
    Some(last_label)
}

/// Expand physical statement lines, coercing space-separated single-liners when detected.
pub fn expand_flattened_program_statements(lines: &[String]) -> FlattenedProgram {
    let mut statements = Vec::new();
    let mut coerced_default_return = None;
    for line in lines {
        let split = split_flattened_program_line(line);
        if let Some(label) = split.coerced_default_return {
            coerced_default_return = Some(label);
        }
        for part in split.statements {
            if !part.trim().is_empty() {
                statements.push(part);
            }
        }
    }
    if coerced_default_return.is_none() {
        coerced_default_return = coerce_binding_only_program_roots(&mut statements);
    }
    FlattenedProgram {
        statements,
        coerced_default_return,
    }
}

