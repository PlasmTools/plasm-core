//! Physical-line program staging and heredoc-aware statement collection.

use super::super::heredoc_surface::{
    heredoc_surface_step_at, tagged_heredoc_close_kind, HeredocSurfaceStep,
};
use super::labels::is_valid_program_label;

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

pub(super) fn plp2_unterminated_heredoc_message(tag: &str, cur: &str) -> String {
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
                "{BASE}; close line not recognized after `{tag}` — if trailing call arguments follow the close tag on the same line, use a close delimiter such as `TAG,` or `TAG)` before the next argument"
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
