//! Physical-line program staging and delimiter-aware splitting shared with DAG lowering.
//!
//! Multi-line Plasm programs must join tagged heredocs across physical lines before binding/root
//! splitting — same rules as structured parameter heredocs ([`super::heredoc_surface`]).

use super::heredoc_surface::{
    heredoc_surface_step_at, tagged_heredoc_close_kind, HeredocSurfaceStep,
};
use std::collections::BTreeSet;

/// Strip trailing `;;` line comments (teaching table-style).
#[inline]
pub fn strip_line_comment(line: &str) -> &str {
    line.split_once(";;").map_or(line, |(left, _)| left)
}

/// One physical line is a complete Plasm program statement, **unless** it opens a tagged heredoc
/// whose closing `TAG` line has not yet been seen (then callers accumulate further physical lines).
#[derive(Debug)]
pub enum PhysicalLineStmtState {
    Complete,
    AwaitingHeredocClose { tag: String },
    AwaitingDelimiterClose,
}

pub fn scan_physical_line_stmt_state(line: &str) -> Result<PhysicalLineStmtState, String> {
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<char>;
    while i < line.len() {
        let c = line[i..]
            .chars()
            .next()
            .ok_or_else(|| "invalid UTF-8 boundary".to_string())?;
        let cl = c.len_utf8();
        if quote.is_none() {
            match heredoc_surface_step_at(line, i)? {
                HeredocSurfaceStep::NotAnOpener => {}
                HeredocSurfaceStep::OpenerIncomplete { tag } => {
                    return Ok(PhysicalLineStmtState::AwaitingHeredocClose { tag });
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
            _ => {}
        }
        i += cl;
    }
    if quote.is_some() {
        return Err(crate::plp::plp3_staging(
            "physical newline inside a quoted Plasm string parameter; use a tagged heredoc for multiline string parameters, e.g. `p58=<<MAIL_7f3a` then the body and a closing `MAIL_7f3a)` line",
        ));
    }
    if depth > 0 {
        return Ok(PhysicalLineStmtState::AwaitingDelimiterClose);
    }
    if depth < 0 {
        return Err(format!(
            "unbalanced delimiters in Plasm program line `{line}`"
        ));
    }
    Ok(PhysicalLineStmtState::Complete)
}

/// Sugar: program-level `label <<TAG` → `label = <<TAG` (same heredoc close rules).
fn normalize_program_binding_heredoc_sugar(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.contains('=') {
        return line.to_string();
    }
    let mut parts = trimmed.split_whitespace();
    let Some(label) = parts.next() else {
        return line.to_string();
    };
    if !is_valid_program_label(label) {
        return line.to_string();
    }
    let rest = trimmed[label.len()..].trim_start();
    if !rest.starts_with("<<") {
        return line.to_string();
    }
    let leading_len = line.len().saturating_sub(trimmed.len());
    format!("{}{label} = {rest}", &line[..leading_len])
}

/// Join physical lines into logical statements, respecting tagged heredocs that span lines.
struct PhysicalLineStatementScanner {
    out: Vec<String>,
    cur: String,
    pending_tag: Option<String>,
    pending_delimiters: bool,
}

impl PhysicalLineStatementScanner {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            cur: String::new(),
            pending_tag: None,
            pending_delimiters: false,
        }
    }

    fn normalize_line(&self, raw: &str) -> String {
        if self.pending_tag.is_some() || self.pending_delimiters {
            strip_line_comment(raw).to_string()
        } else {
            normalize_program_binding_heredoc_sugar(strip_line_comment(raw))
        }
    }

    fn apply_stmt_state(&mut self, state: PhysicalLineStmtState) -> Result<(), String> {
        match state {
            PhysicalLineStmtState::Complete => {
                self.out.push(self.cur.trim_end().to_string());
                self.cur.clear();
                self.pending_delimiters = false;
                Ok(())
            }
            PhysicalLineStmtState::AwaitingHeredocClose { tag } => {
                self.pending_tag = Some(tag);
                self.pending_delimiters = false;
                Ok(())
            }
            PhysicalLineStmtState::AwaitingDelimiterClose => {
                self.pending_delimiters = true;
                Ok(())
            }
        }
    }

    fn push_line(&mut self, raw: &str) -> Result<(), String> {
        let w = self.normalize_line(raw);
        if self.pending_tag.is_some() || self.pending_delimiters {
            if !self.cur.is_empty() {
                self.cur.push('\n');
            }
            self.cur.push_str(&w);
            if let Some(tag) = self.pending_tag.as_deref() {
                let last = self.cur.lines().last().unwrap_or("");
                if tagged_heredoc_close_kind(last, tag).is_none() {
                    return Ok(());
                }
                self.pending_tag = None;
            }
            self.apply_stmt_state(scan_physical_line_stmt_state(&self.cur)?)?;
            return Ok(());
        }

        if w.trim().is_empty() {
            return Ok(());
        }
        self.cur.clear();
        self.cur.push_str(&w);
        self.apply_stmt_state(scan_physical_line_stmt_state(&self.cur)?)?;
        Ok(())
    }

    fn finish(self) -> Result<Vec<String>, String> {
        if let Some(tag) = self.pending_tag.as_deref() {
            return Err(plp2_unterminated_heredoc_message(tag, &self.cur));
        }
        if self.pending_delimiters {
            return Err(crate::plp::plp3_staging(
                "unterminated Plasm program statement (unbalanced delimiters after heredoc close)",
            ));
        }
        if !self.cur.is_empty() {
            return Err(crate::plp::plp3_staging(
                "unterminated Plasm program statement (unexpected trailing fragment)",
            ));
        }
        Ok(self.out)
    }
}

pub fn collect_program_statement_lines(src: &str) -> Result<Vec<String>, String> {
    let mut scanner = PhysicalLineStatementScanner::new();
    for raw in src.lines() {
        scanner.push_line(raw)?;
    }
    scanner.finish()
}

fn plp2_unterminated_heredoc_message(tag: &str, cur: &str) -> String {
    const BASE: &str = "unterminated tagged heredoc (missing closing `TAG` line, or missing newline after `<<TAG` on the opener line)";
    let lines: Vec<&str> = cur.lines().collect();
    if let Some(last) = lines.last() {
        if tagged_heredoc_close_kind(last, tag).is_some() {
            return crate::plp::plp2_heredoc(format!(
                "{BASE}; close line `{last}` should have ended the heredoc but program staging still has pending tag `{tag}` (file an issue)"
            ));
        }
        let trimmed = last.trim();
        if trimmed.starts_with(tag) {
            return crate::plp::plp2_heredoc(format!(
                "{BASE}; close line not recognized after `{tag}` — if trailing call arguments follow the close tag on the same line, ensure the host is plasm-core ≥0.3.126"
            ));
        }
    }
    for line in lines.iter().take(lines.len().saturating_sub(1)) {
        if line.trim() == tag {
            return crate::plp::plp2_heredoc(format!(
                "{BASE}; body contains a line equal to close tag `{tag}` before the real close — use an opaque tag that cannot appear in the payload"
            ));
        }
    }
    crate::plp::plp2_heredoc(BASE)
}

pub fn looks_like_domain_symbol(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some('e' | 'p' | 'm' | 'r'))
        && matches!(chars.next(), Some(c) if c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_digit())
}

/// Valid identifier for a program binding label (not `e1`/`p2`-style teaching symbols).
pub fn is_valid_program_label(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !looks_like_domain_symbol(label)
}

pub fn validate_program_label(label: &str) -> Result<(), String> {
    if !is_valid_program_label(label) || matches!(label, "_" | "$" | "return") {
        return Err(program_invalid_binding_label_error(label));
    }
    Ok(())
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

fn find_flattened_assignment_split(rhs: &str) -> Option<usize> {
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
    if line.is_empty() || line.contains("<<") || !line_has_flattened_program_shape(line) {
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

fn leading_identifier(s: &str) -> &str {
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

/// Agent-facing hint when a program has bindings but no executable return roots.
pub fn missing_program_roots_error() -> String {
    "Add a final return line (e.g. `limited[p2,p14]`), or omit only when every line is `label = …` (last binding is returned)."
        .to_string()
}

pub fn program_empty_error() -> String {
    "Program is empty.".to_string()
}

pub fn program_return_keyword_error() -> String {
    "Remove `return` — write bare roots on the last line (e.g. `limited` or `a, b`).".to_string()
}

pub fn program_invalid_binding_label_error(label: &str) -> String {
    if looks_like_domain_symbol(label) {
        format!("Binding names must be labels like `issue`, not teaching symbols (`{label}`).")
    } else {
        format!("Binding names must be identifiers like `issue`, not `{label}`.")
    }
}

pub fn program_binding_after_return_error() -> String {
    "Return must be last — move bindings above the return line, or bind intermediate steps before returning."
        .to_string()
}

pub fn program_multiple_return_lines_error() -> String {
    "Only one return line allowed — put every root on one comma-separated line (e.g. `a, b, c`), not one bare label per line."
        .to_string()
}

pub fn program_intermediate_return_error(stmt: &str) -> String {
    let stmt = stmt.trim();
    format!(
        "Only one return line allowed — bind this step first (e.g. `filtered = {stmt}`), then end with one return line."
    )
}

pub fn program_intermediate_return_must_be_binding_error(stmt: &str) -> String {
    let head = leading_identifier(stmt.trim());
    format!(
        "Intermediate step must be a binding — write `{head} = {stmt}` (not a bare `{stmt}` line), then return on the last line."
    )
}

pub fn program_duplicate_return_node_error() -> String {
    "Program has multiple return expressions — bind each step (`filtered = comments.filter{{…}}`), then one final return line."
        .to_string()
}

/// ML `let` block: bindings first, one return last. Rejects multiple roots-only lines and bindings after return.
pub fn validate_program_statement_order(statements: &[String]) -> Result<(), String> {
    let stmts: Vec<&str> = statements
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut bindings: BTreeSet<String> = BTreeSet::new();
    let mut saw_roots = false;
    let n = stmts.len();
    for (i, stmt) in stmts.iter().enumerate() {
        let is_last = i + 1 == n;
        if let Some((label, _)) = split_assignment_for_binding(stmt) {
            if saw_roots {
                return Err(program_binding_after_return_error());
            }
            bindings.insert(label.to_string());
            continue;
        }
        if saw_roots && !is_last {
            if is_bare_binding_label(stmt, &bindings) {
                return Err(program_multiple_return_lines_error());
            }
            return Err(program_intermediate_return_error(stmt));
        }
        if !is_last && roots_line_is_postfix_on_binding(stmt, &bindings) {
            return Err(program_intermediate_return_must_be_binding_error(stmt));
        }
        saw_roots = true;
    }
    Ok(())
}

fn is_bare_binding_label(stmt: &str, bindings: &BTreeSet<String>) -> bool {
    let stmt = stmt.trim();
    let head = leading_identifier(stmt);
    !head.is_empty() && head.len() == stmt.len() && bindings.contains(head)
}

fn roots_line_is_postfix_on_binding(stmt: &str, bindings: &BTreeSet<String>) -> bool {
    let stmt = stmt.trim();
    let head = leading_identifier(stmt);
    if head.is_empty() || head.len() == stmt.len() {
        return false;
    }
    if !is_valid_program_label(head) {
        return false;
    }
    if !bindings.contains(head) {
        return false;
    }
    let rest = stmt[head.len()..].trim_start();
    if rest.is_empty() || rest.starts_with('[') {
        return false;
    }
    rest.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_top_level_keeps_commas_inside_tagged_heredoc() {
        let parts = split_top_level("fn(<<T\na,b,c\nT\n), bar", ',').expect("split");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("a,b,c"));
        assert_eq!(parts[1].trim(), "bar");
    }

    #[test]
    fn split_assignment_respects_heredoc_bodies_with_equals() {
        let stmt = "body = <<PLASM_EOF\nkey = value\nPLASM_EOF";
        let (label, rhs) = split_assignment_at_top_level(stmt).expect("assignment");
        assert_eq!(label, "body");
        assert!(rhs.starts_with("<<PLASM_EOF"));
        assert!(rhs.contains("key = value"));
    }

    #[test]
    fn collect_program_binding_heredoc_sugar_without_equals() {
        let src = "body <<PLASM_LABEL_ISSUE_V1\n## Problem\nline two\nPLASM_LABEL_ISSUE_V1\ncreated = x()\nbody, created";
        let stmts = collect_program_statement_lines(src).expect("parse");
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].starts_with("body = <<PLASM_LABEL_ISSUE_V1"));
        assert!(stmts[0].contains("## Problem"));
        assert_eq!(stmts[1], "created = x()");
        assert_eq!(stmts[2], "body, created");
    }

    #[test]
    fn collect_program_statement_lines_errors_on_squashed_heredoc_opener() {
        let err = collect_program_statement_lines("body = <<B # junk").expect_err("err");
        assert!(
            err.contains("PLP-2:") || err.contains("PLP-3:"),
            "unexpected err: {err}"
        );
        assert!(
            err.contains("tagged heredoc") || err.contains("<<"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn collect_program_statement_lines_glued_heredoc_close() {
        let stmts = collect_program_statement_lines("x = m(<<H\none\nH)").expect("parse");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("<<H"));
        assert!(stmts[0].contains("one"));
    }

    #[test]
    fn collect_program_statement_lines_waits_for_delimiters_after_heredoc_close() {
        let src = "x = m(v111{content=<<H\none\nH\n})\nx";
        let stmts = collect_program_statement_lines(src).expect("parse");
        assert_eq!(stmts, vec!["x = m(v111{content=<<H\none\nH\n})", "x"]);
    }

    #[test]
    fn collect_mid_call_heredoc_same_line_trailing_args() {
        let src = r#"created = e1.m1(p86="title", p73=<<PLASM_BODY_7f3a
## Problem
Testing.
PLASM_BODY_7f3a, p28=["documentation"])
created"#;
        let stmts = collect_program_statement_lines(src).expect("parse");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("<<PLASM_BODY_7f3a"));
        assert!(stmts[0].contains("## Problem"));
        assert!(stmts[0].contains("p28=[\"documentation\"]"));
        assert_eq!(stmts[1], "created");
    }

    #[test]
    fn collect_mid_call_heredoc_next_line_trailing_args() {
        let src = r#"created = LangItem.create(title=<<PLASM_INLINE_ARG
line one
PLASM_INLINE_ARG
, score=0, owner="inline-heredoc")
created"#;
        let stmts = collect_program_statement_lines(src).expect("parse");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("<<PLASM_INLINE_ARG"));
        assert!(stmts[0].contains(", score=0"));
        assert_eq!(stmts[1], "created");
    }

    #[test]
    fn plp2_finish_hints_unrecognized_same_line_close() {
        let err = collect_program_statement_lines("x = m(body=<<BODY\nline\nBODYfoo, other=1)")
            .expect_err("staging should fail when close suffix is not delimiter-only");
        assert!(err.contains("PLP-2:"), "{err}");
        assert!(err.contains("close line not recognized"), "{err}");
    }

    #[test]
    fn plp2_message_tag_collision_hint() {
        let msg = plp2_unterminated_heredoc_message(
            "TAG",
            "opener <<TAG\nTAG\nbody\nnot_closed",
        );
        assert!(msg.contains("PLP-2:"), "{msg}");
        assert!(
            msg.contains("body contains a line equal to close tag"),
            "{msg}"
        );
    }

    #[test]
    fn collect_program_statement_lines_inline_heredoc_in_call() {
        let src = "created = e1.m1(p86=\"title\",\n  p73=<<PLASM_INLINE\n## Problem\nline\nPLASM_INLINE\n)\ncreated";
        let stmts = collect_program_statement_lines(src).expect("parse");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("e1.m1("));
        assert!(stmts[0].contains("<<PLASM_INLINE"));
        assert!(stmts[0].contains("## Problem"));
        assert_eq!(stmts[1], "created");
    }

    #[test]
    fn split_token_top_level_respects_nesting() {
        let got1 = split_token_top_level("src => Effect(x)", "=>").expect("ok");
        assert_eq!(
            got1.map(|(a, b)| (a.trim(), b.trim())),
            Some(("src", "Effect(x)"))
        );
        let got2 = split_token_top_level("(a=>b) => c", "=>").expect("ok");
        assert_eq!(
            got2.map(|(a, b)| (a.trim(), b.trim())),
            Some(("(a=>b)", "c"))
        );
    }

    #[test]
    fn split_assignment_skips_effect_arrow() {
        assert!(split_assignment_at_top_level("pika => e2.r3").is_none());
        assert!(split_assignment_for_binding("sync = items => e1.m1(p1=1)").is_some());
    }

    #[test]
    fn rejects_domain_symbol_labels_for_assignment_split() {
        assert!(split_assignment_for_binding("e1 = foo").is_none());
        assert!(split_assignment_for_binding("repo = x").is_some());
    }

    #[test]
    fn split_flattened_program_line_preserves_for_each_effect_binding() {
        let line = "sync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)";
        let split = split_flattened_program_line(line);
        assert_eq!(split.statements.len(), 1);
        assert_eq!(split.statements[0], line);
        assert!(split.coerced_default_return.is_none());
    }

    #[test]
    fn split_flattened_program_line_bindings_and_primary_return() {
        let split =
            split_flattened_program_line("issues = e1{p1=open} labels = issues.labels labels");
        assert_eq!(split.statements.len(), 3);
        assert!(split.statements[0].starts_with("issues = "));
        assert!(split.statements[1].starts_with("labels = "));
        assert_eq!(split.statements[2], "issues");
        assert_eq!(split.coerced_default_return.as_deref(), Some("issues"));
    }

    #[test]
    fn expand_flattened_program_surfaces_coerced_return() {
        let expanded = expand_flattened_program_statements(&[
            "repo = e1 issues = e2 labels = issues.labels labels".to_string(),
        ]);
        assert_eq!(expanded.coerced_default_return.as_deref(), Some("repo"));
        assert!(expanded.statements.iter().any(|s| s.starts_with("repo = ")));
    }

    #[test]
    fn expand_binding_only_newline_separated_coerces_last_binding_return() {
        let expanded = expand_flattened_program_statements(&[
            "hits = e4(p1=\"sha\")".to_string(),
            "labels = hits.p5".to_string(),
        ]);
        assert_eq!(expanded.coerced_default_return.as_deref(), Some("labels"));
        assert_eq!(
            expanded.statements.last().map(String::as_str),
            Some("labels")
        );
    }

    #[test]
    fn expand_single_binding_line_coerces_default_return() {
        let expanded = expand_flattened_program_statements(&["hits = e4(p1=\"sha\")".to_string()]);
        assert_eq!(expanded.coerced_default_return.as_deref(), Some("hits"));
        assert_eq!(expanded.statements.last().map(String::as_str), Some("hits"));
    }

    #[test]
    fn expand_multiline_explicit_non_first_root_preserved() {
        let expanded = expand_flattened_program_statements(&[
            "issue = e2(p4=\"PLA-1\")".to_string(),
            "comments = issue.r2".to_string(),
            "limited = comments.limit(5)".to_string(),
            "limited[p2,p14]".to_string(),
        ]);
        assert!(expanded.coerced_default_return.is_none());
        assert_eq!(
            expanded.statements.last().map(String::as_str),
            Some("limited[p2,p14]")
        );
    }

    #[test]
    fn expand_multiline_explicit_side_label_root_preserved() {
        let expanded = expand_flattened_program_statements(&[
            "repo = e1".to_string(),
            "labels = e2".to_string(),
            "labels".to_string(),
        ]);
        assert!(expanded.coerced_default_return.is_none());
        assert_eq!(
            expanded.statements.last().map(String::as_str),
            Some("labels")
        );
    }

    #[test]
    fn split_flattened_line_keeps_projection_on_in_scope_binding() {
        let split = split_flattened_program_line(
            "issue = e2(p4=\"PLA-1\") comments = issue.r2 comments[p2,p14]",
        );
        assert_eq!(
            split.statements.last().map(String::as_str),
            Some("comments[p2,p14]")
        );
        assert!(split.coerced_default_return.is_none());
    }

    #[test]
    fn split_flattened_line_keeps_postfix_projection_on_in_scope_binding() {
        let split = split_flattened_program_line(
            "issue = e2(p4=\"PLA-1\") comments = issue.r2 comments.limit(5)[p2,p14]",
        );
        assert_eq!(
            split.statements.last().map(String::as_str),
            Some("comments.limit(5)[p2,p14]")
        );
        assert!(split.coerced_default_return.is_none());
    }

    #[test]
    fn split_flattened_line_keeps_projection_on_first_binding() {
        let split = split_flattened_program_line(
            "issue = e2(p4=\"PLA-1\") comments = issue.r2 issue[p4,p19]",
        );
        assert_eq!(
            split.statements.last().map(String::as_str),
            Some("issue[p4,p19]")
        );
        assert!(split.coerced_default_return.is_none());
    }

    #[test]
    fn split_flattened_line_fresh_trailing_query_still_coerces() {
        let split = split_flattened_program_line(
            "item = LangItem(\"i1\") LangItem.sort(score, desc).limit(2)",
        );
        assert_eq!(split.statements.last().map(String::as_str), Some("item"));
        assert_eq!(split.coerced_default_return.as_deref(), Some("item"));
    }

    #[test]
    fn validate_rejects_intermediate_postfix_without_binding() {
        let err = validate_program_statement_order(&[
            "comments = issue.r2".to_string(),
            "comments.filter{p14=\"a\"}".to_string(),
            "comments[p2,p14]".to_string(),
        ])
        .expect_err("must bind intermediate postfix");
        assert!(
            err.contains("binding") || err.contains("Intermediate"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_multiple_bare_root_lines() {
        let err = validate_program_statement_order(&[
            "a = e1".to_string(),
            "b = e2".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ])
        .expect_err("multiple return lines");
        assert!(err.contains("comma-separated"), "{err}");
    }

    #[test]
    fn validate_rejects_binding_after_return_line() {
        let err = validate_program_statement_order(&[
            "limited[p2,p14]".to_string(),
            "comments = issue.r2".to_string(),
        ])
        .expect_err("binding after return");
        assert!(err.contains("Return must be last"), "{err}");
    }

    #[test]
    fn validate_allows_bare_label_then_projection() {
        validate_program_statement_order(&[
            "comments = issue.r2".to_string(),
            "comments".to_string(),
            "comments[p2,p14]".to_string(),
        ])
        .expect("bare label before projection on last line");
    }
}
