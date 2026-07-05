//! Agent-facing compaction for large Plasm surface expressions in dry-run display.

use std::fmt::Write as _;

/// Shrink large literal/heredoc bodies in agent-facing dry-run step lines.
pub(crate) fn compact_agent_surface_expr(expr: &str) -> String {
    const INLINE_MAX: usize = 256;
    const HEAD: usize = 64;
    if expr.chars().count() <= INLINE_MAX {
        return expr.to_string();
    }
    let mut out = String::new();
    let mut rest = expr;
    while !rest.is_empty() {
        let Some(open) = rest.find("<<") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let tag_end = after_open
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after_open.len());
        if tag_end == 0 {
            out.push_str("<<");
            rest = after_open;
            continue;
        }
        let tag = &after_open[..tag_end];
        let body_start = open + 2 + tag_end;
        let body_rest = &rest[body_start..];
        let body_rest = body_rest.strip_prefix('\n').unwrap_or(body_rest);
        if let Some(close_idx) = body_rest.lines().position(|line| line.trim() == tag) {
            let inner_len: usize = body_rest
                .lines()
                .take(close_idx)
                .map(|l| l.chars().count() + 1)
                .sum();
            let _ = write!(
                out,
                "<<{tag} … ({inner_len} chars, full in run artifact) … {tag}"
            );
            let consumed = body_start
                + body_rest
                    .lines()
                    .take(close_idx + 1)
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .min(body_rest.len());
            rest = &rest[consumed..];
            continue;
        }
        out.push_str("<<");
        rest = after_open;
    }
    summarize_agent_surface_literal(&out, INLINE_MAX, HEAD)
}

fn summarize_agent_surface_literal(s: &str, inline_max: usize, head: usize) -> String {
    let n = s.chars().count();
    if n <= inline_max {
        return s.to_string();
    }
    let head_s: String = s.chars().take(head).collect();
    format!("{head_s}… ({n} chars, full in run artifact)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_agent_surface_expr_summarizes_large_heredoc() {
        let body = "x\n".repeat(200);
        let expr = format!("label <<TAG\n{body}TAG");
        let compact = compact_agent_surface_expr(&expr);
        assert!(compact.contains("…"));
        assert!(compact.contains("full in run artifact"));
        assert!(!compact.contains(&body));
    }
}
