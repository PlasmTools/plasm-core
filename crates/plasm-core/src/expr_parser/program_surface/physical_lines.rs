//! Physical-line program staging and heredoc-aware statement collection.

use super::super::heredoc_surface::{
    heredoc_surface_step_at, tagged_heredoc_close_kind, HeredocSurfaceStep,
};
use super::labels::is_valid_program_label;
use super::SurfaceSyntaxError;

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

pub fn scan_physical_line_stmt_state(
    line: &str,
) -> Result<PhysicalLineStmtState, SurfaceSyntaxError> {
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<char>;
    while i < line.len() {
        let c = line[i..]
            .chars()
            .next()
            .ok_or(SurfaceSyntaxError::InvalidUtf8Boundary)?;
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
        return Err(SurfaceSyntaxError::QuotedPhysicalNewline);
    }
    if depth > 0 {
        return Ok(PhysicalLineStmtState::AwaitingDelimiterClose);
    }
    if depth < 0 {
        return Err(SurfaceSyntaxError::UnbalancedDelimiters {
            expression: line.to_owned(),
        });
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

    fn apply_stmt_state(&mut self, state: PhysicalLineStmtState) -> Result<(), SurfaceSyntaxError> {
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

    fn push_line(&mut self, raw: &str) -> Result<(), SurfaceSyntaxError> {
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

    fn finish(self) -> Result<Vec<String>, SurfaceSyntaxError> {
        if let Some(tag) = self.pending_tag.as_deref() {
            return Err(plp2_unterminated_heredoc_message(tag, &self.cur));
        }
        if self.pending_delimiters {
            return Err(SurfaceSyntaxError::StatementDelimitersUnterminated);
        }
        if !self.cur.is_empty() {
            return Err(SurfaceSyntaxError::StatementTrailingFragment);
        }
        Ok(self.out)
    }
}

pub fn collect_program_statement_lines(src: &str) -> Result<Vec<String>, SurfaceSyntaxError> {
    let mut scanner = PhysicalLineStatementScanner::new();
    for raw in src.lines() {
        scanner.push_line(raw)?;
    }
    scanner.finish()
}

pub(super) fn plp2_unterminated_heredoc_message(tag: &str, cur: &str) -> SurfaceSyntaxError {
    let lines: Vec<&str> = cur.lines().collect();
    if let Some(last) = lines.last() {
        if tagged_heredoc_close_kind(last, tag).is_some() {
            return SurfaceSyntaxError::HeredocCloseNotStaged {
                tag: tag.to_owned(),
                line: (*last).to_owned(),
            };
        }
        let trimmed = last.trim();
        if trimmed.starts_with(tag) {
            return SurfaceSyntaxError::HeredocCloseDelimiterMissing {
                tag: tag.to_owned(),
            };
        }
    }
    for line in lines.iter().take(lines.len().saturating_sub(1)) {
        if line.trim() == tag {
            return SurfaceSyntaxError::HeredocTagCollision {
                tag: tag.to_owned(),
            };
        }
    }
    SurfaceSyntaxError::HeredocUnterminated {
        tag: tag.to_owned(),
    }
}
