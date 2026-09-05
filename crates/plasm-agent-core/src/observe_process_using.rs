//! Observe-site process-using footer (F_m0) for plural identity / large TSV dumps.
//!
//! Host observation pressure — **not** RA-8 coerce. After inline ` ```tsv ` fences that
//! look like fanout-relevant row dumps, append a token-minimal didactic line:
//! `rows => e#.m#(…, _.wire) — not N applies`
//!
//! Payload inside the fence is untouched. Idempotent if a footer is already present.
//! Canonical product path: [`crate::http_execute::mcp_publish`] via [`append_tsv_process_using_footers`].

use std::collections::{HashMap, HashSet};

const OBS_DISTINCT_MAX: usize = 8;

/// Columns that mark a TSV as identity-bearing (fanout-relevant) for F_m0.
const OBS_IDENTITY_COLS: &[&str] = &[
    "path",
    "source_file_path",
    "destination_file_path",
    "file_path",
    "directory_path",
    "uri",
    "url",
];

fn col_is_identity(name: &str) -> bool {
    let hl = name.trim().to_ascii_lowercase();
    if hl.is_empty() {
        return false;
    }
    if OBS_IDENTITY_COLS.iter().any(|c| *c == hl) {
        return true;
    }
    hl.ends_with("_path") || hl.ends_with("_uri") || hl.ends_with("_url")
}

struct TsvBodyStats {
    n_rows: usize,
    headers: Vec<String>,
    identity_headers: Vec<String>,
    high_card_identity: bool,
}

fn tsv_body_stats(body: &str) -> TsvBodyStats {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim_end)
        .filter(|ln| !ln.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return TsvBodyStats {
            n_rows: 0,
            headers: Vec::new(),
            identity_headers: Vec::new(),
            high_card_identity: false,
        };
    }
    let headers: Vec<String> = lines[0]
        .split('\t')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .collect();
    let mut col_vals: HashMap<String, HashSet<String>> = headers
        .iter()
        .map(|h| (h.clone(), HashSet::new()))
        .collect();
    let mut n_rows = 0usize;
    for ln in &lines[1..] {
        if ln.trim_start().starts_with('#') {
            continue;
        }
        n_rows += 1;
        let cells: Vec<&str> = ln.split('\t').collect();
        for (i, h) in headers.iter().enumerate() {
            if let Some(cell) = cells.get(i) {
                let val = cell.trim();
                if !val.is_empty() && val != "(in artifact)" {
                    // Keys are seeded from `headers` above — insert only if present.
                    if let Some(set) = col_vals.get_mut(h) {
                        set.insert(val.to_string());
                    }
                }
            }
        }
    }
    let identity_headers: Vec<String> = headers
        .iter()
        .filter(|h| col_is_identity(h))
        .cloned()
        .collect();
    let high_card_identity = col_vals.iter().any(|(h, vals)| {
        col_is_identity(h) && (vals.len() > OBS_DISTINCT_MAX || n_rows > OBS_DISTINCT_MAX)
    });
    TsvBodyStats {
        n_rows,
        headers,
        identity_headers,
        high_card_identity,
    }
}

fn min_shape_slot(identity_headers: &[String], headers: &[String]) -> String {
    let wires: Vec<&str> = if !identity_headers.is_empty() {
        identity_headers.iter().map(String::as_str).collect()
    } else {
        headers
            .iter()
            .filter(|h| col_is_identity(h))
            .map(String::as_str)
            .collect()
    };
    let slot = wires
        .first()
        .map(|w| format!("_.{w}"))
        .unwrap_or_else(|| "_.f".to_string());
    format!("rows => e#.m#(…, {slot})")
}

/// F_m0 footer line (`rows => e#.m#(…, _.wire) — not N applies`).
pub fn format_fm0_process_using_footer(
    identity_headers: &[String],
    headers: &[String],
) -> String {
    let shape = min_shape_slot(identity_headers, headers);
    format!("{shape} — not N applies")
}

fn footer_already_present(after: &str) -> bool {
    let a = after.to_ascii_lowercase();
    a.contains("process using:")
        || a.contains("do not copy cell literals into n applies")
        || a.contains("one `=>` fanout")
        || a.contains("one => fanout")
        || a.contains(" — not n applies")
        || a.contains("rows → rows =>")
        || a.contains("rows => e#")
}

/// Find next ` ```tsv ` … ` ``` ` fence starting at `from` (case-insensitive open).
fn next_tsv_fence(text: &str, from: usize) -> Option<(usize, usize, usize)> {
    let lower = text[from..].to_ascii_lowercase();
    let open_rel = lower.find("```tsv")?;
    let open = from + open_rel;
    let after_open = open + "```tsv".len();
    // skip optional spaces then require newline
    let body_start = {
        let rest = &text[after_open..];
        let trimmed = rest.trim_start_matches([' ', '\t']);
        let skipped = rest.len() - trimmed.len();
        if !trimmed.starts_with('\n') && !trimmed.starts_with("\r\n") {
            // malformed — search further
            return next_tsv_fence(text, after_open);
        }
        let nl = if trimmed.starts_with("\r\n") { 2 } else { 1 };
        after_open + skipped + nl
    };
    let close_rel = text[body_start..].find("```")?;
    let close = body_start + close_rel;
    let fence_end = close + 3;
    Some((open, body_start, fence_end))
}

/// After each plural identity (or large) TSV fence, append F_m0. Payload untouched.
pub fn append_tsv_process_using_footers(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut last = 0usize;
    let mut cursor = 0usize;
    while let Some((_open, body_start, fence_end)) = next_tsv_fence(text, cursor) {
        out.push_str(&text[last..fence_end]);
        let body = &text[body_start..fence_end - 3];
        let st = tsv_body_stats(body);
        let want = st.n_rows > 1
            && (st.high_card_identity
                || !st.identity_headers.is_empty()
                || st.n_rows > OBS_DISTINCT_MAX);
        let after_end = (fence_end + 120).min(text.len());
        let after = &text[fence_end..after_end];
        if want && !footer_already_present(after) {
            let line = format_fm0_process_using_footer(&st.identity_headers, &st.headers);
            out.push('\n');
            out.push_str(&line);
            out.push('\n');
        }
        last = fence_end;
        cursor = fence_end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm0_after_plural_path_tsv() {
        let mut paths = vec!["path\tflag".to_string()];
        for i in 1..=11 {
            paths.push(format!("/zone/work/item_{i:02}.dat\ttrue"));
        }
        let path_tsv = format!("## files (11 rows)\n```tsv\n{}\n```\n", paths.join("\n"));
        let footed = append_tsv_process_using_footers(&path_tsv);
        assert!(footed.contains("/zone/work/item_01"));
        assert!(footed.contains("rows => e#") && footed.contains("not N applies"));
        assert!(!footed.contains("done ="));
        assert!(!footed.contains("process using:"));
        assert!(footed.find("```").unwrap() < footed.find("rows =>").unwrap());
        assert_eq!(footed.matches("not N applies").count(), 1);
        assert_eq!(
            append_tsv_process_using_footers(&footed)
                .matches("not N applies")
                .count(),
            1
        );
    }

    #[test]
    fn skips_non_identity_small_dump() {
        let tsv = "## status (2 rows)\n```tsv\nstate\tok\npending\n```\n";
        let out = append_tsv_process_using_footers(tsv);
        assert!(!out.contains("not N applies"));
        assert_eq!(out, tsv);
    }
}
