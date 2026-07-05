//! Tagged heredoc recognition shared by value parsing, postfix render tails, and multi-line program scans.

/// How a structured heredoc closing line was recognized (tagged `TAG` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeredocCloseLineKind {
    LineOnly,
    GluedSuffix,
}

/// Tagged heredoc close: trim matches `TAG` alone, or `TAG` followed by optional ASCII ws and
/// either a comma (more call arguments on the same line) or one or more parser-owned delimiters
/// (`)`, `]`, `}`, `,`). The heredoc recognizer closes the string at `TAG` and leaves the suffix
/// for the enclosing parser to validate.
pub fn tagged_heredoc_close_kind(
    line_slice: &str,
    tag: &str,
) -> Option<(HeredocCloseLineKind, usize)> {
    let leading_ws = line_slice.len() - line_slice.trim_start().len();
    let t = line_slice.trim();
    if t == tag {
        return Some((HeredocCloseLineKind::LineOnly, leading_ws));
    }
    if !t.starts_with(tag) {
        return None;
    }
    let after = t[tag.len()..].trim_start();
    if after.is_empty() {
        return None;
    }
    if after.starts_with(',') {
        return Some((HeredocCloseLineKind::GluedSuffix, leading_ws));
    }
    let mut saw_delimiter = false;
    for b in after.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if matches!(b, b')' | b']' | b'}' | b',') {
            saw_delimiter = true;
            continue;
        }
        return None;
    }
    if saw_delimiter {
        return Some((HeredocCloseLineKind::GluedSuffix, leading_ws));
    }
    None
}

pub(crate) enum HeredocOpener {
    /// Parsed `<<TAG` but the opener line does not yet contain the required newline after `TAG`
    /// (physical line split — accumulate more lines into the same statement).
    Incomplete {
        tag: String,
    },
    Complete {
        tag: String,
        body_start: usize,
    },
}

#[inline]
pub fn is_tagged_heredoc_opener_start(bytes: &[u8], i: usize) -> bool {
    i + 2 <= bytes.len()
        && &bytes[i..i + 2] == b"<<"
        && !(i + 3 <= bytes.len() && bytes[i + 2] == b'<')
}

/// What to do at byte index `i` when the surface scan sees a potential `<<TAG` heredoc.
pub enum HeredocSurfaceStep {
    /// Not `<<` / `<<<` — caller advances one UTF-8 scalar.
    NotAnOpener,
    /// Jump `i` to this index (past full heredoc).
    SkipTo(usize),
    /// Opener `<<TAG` has no newline after tag on this fragment (physical line continues later).
    OpenerIncomplete { tag: String },
}

/// Unified tagged-heredoc recognition for Plasm program surface scans (`split_top_level`, statement line scan).
pub fn heredoc_surface_step_at(s: &str, i: usize) -> Result<HeredocSurfaceStep, String> {
    let b = s.as_bytes();
    if !is_tagged_heredoc_opener_start(b, i) {
        return Ok(HeredocSurfaceStep::NotAnOpener);
    }
    match try_parse_tagged_heredoc_opener(s, i)? {
        HeredocOpener::Incomplete { tag } => Ok(HeredocSurfaceStep::OpenerIncomplete { tag }),
        HeredocOpener::Complete { tag, body_start } => {
            let end = skip_tagged_structured_heredoc(s, body_start, &tag)?;
            Ok(HeredocSurfaceStep::SkipTo(end))
        }
    }
}

/// Parse `<<TAG` on the line containing `open_idx` (byte index of first `<`), requiring a newline
/// after the tag on the same line with only ASCII whitespace between tag and newline.
pub fn try_parse_tagged_heredoc_opener(s: &str, open_idx: usize) -> Result<HeredocOpener, String> {
    let b = s.as_bytes();
    debug_assert!(is_tagged_heredoc_opener_start(b, open_idx));
    let mut p = open_idx + 2;
    if p >= b.len() {
        return Err(
            "tagged heredoc `<<` must be immediately followed by a tag (`TAG` = [A-Za-z_][A-Za-z0-9_]*) and a newline after the tag on the same line".into(),
        );
    }
    if !(b[p].is_ascii_alphabetic() || b[p] == b'_') {
        return Err(
            "tagged heredoc `<<` must be immediately followed by a tag (`TAG` = [A-Za-z_][A-Za-z0-9_]*) and a newline after the tag on the same line".into(),
        );
    }
    let tag_start = p;
    p += 1;
    while p < b.len() && (b[p].is_ascii_alphanumeric() || b[p] == b'_') {
        p += 1;
    }
    let tag = s[tag_start..p].to_string();
    let Some(line_end_rel) = s[open_idx..].find('\n') else {
        return Ok(HeredocOpener::Incomplete { tag });
    };
    let line_end = open_idx + line_end_rel;
    let tail = s[p..line_end].trim();
    if !tail.is_empty() {
        return Err(format!(
            "tagged heredoc `<<{tag}` opener must be only `<<{tag}` then optional ASCII spaces/tabs before the newline; do not put text (or `#` comments) after the tag on the opener line"
        ));
    }
    Ok(HeredocOpener::Complete {
        tag,
        body_start: line_end + 1,
    })
}

/// Skip from `body_start` through the closing line for `<<TAG`, returning the exclusive end byte index.
pub fn skip_tagged_structured_heredoc(
    s: &str,
    body_start: usize,
    tag: &str,
) -> Result<usize, String> {
    let mut pos = body_start;
    while pos <= s.len() {
        let line_end = s[pos..].find('\n').map(|r| pos + r).unwrap_or(s.len());
        let line_slice = &s[pos..line_end];
        if let Some((kind, leading_ws)) = tagged_heredoc_close_kind(line_slice, tag) {
            return Ok(match kind {
                HeredocCloseLineKind::LineOnly => {
                    if line_end < s.len() {
                        line_end + 1
                    } else {
                        s.len()
                    }
                }
                HeredocCloseLineKind::GluedSuffix => pos + leading_ws + tag.len(),
            });
        }
        if line_end >= s.len() {
            return Err(crate::plp::plp2_heredoc(format!(
                "unterminated tagged heredoc <<{tag}"
            )));
        }
        pos = line_end + 1;
    }
    Err(crate::plp::plp2_heredoc(format!(
        "unterminated tagged heredoc <<{tag}"
    )))
}

/// Parse a standalone tagged heredoc (`<<TAG` … `TAG`) into its body string (no tag delimiters).
pub fn parse_tagged_heredoc_literal(s: &str) -> Result<String, String> {
    let s = s.trim();
    let opener = try_parse_tagged_heredoc_opener(s, 0)?;
    let HeredocOpener::Complete { tag, body_start } = opener else {
        return Err(crate::plp::plp2_heredoc(
            "incomplete tagged heredoc opener (missing newline after <<TAG)",
        ));
    };
    let mut pos = body_start;
    while pos <= s.len() {
        let line_end = s[pos..].find('\n').map(|r| pos + r).unwrap_or(s.len());
        let line_slice = &s[pos..line_end];
        if tagged_heredoc_close_kind(line_slice, tag.as_str()).is_some() {
            return Ok(s[body_start..pos].to_string());
        }
        if line_end >= s.len() {
            break;
        }
        pos = line_end + 1;
    }
    Err(crate::plp::plp2_heredoc(format!(
        "unterminated tagged heredoc <<{tag}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_close_line_only_and_glued() {
        assert!(matches!(
            tagged_heredoc_close_kind("TAG", "TAG"),
            Some((HeredocCloseLineKind::LineOnly, _))
        ));
        for line in ["TAG)", "TAG})", "TAG ],", "TAG } )"] {
            assert!(matches!(
                tagged_heredoc_close_kind(line, "TAG"),
                Some((HeredocCloseLineKind::GluedSuffix, _))
            ));
        }
        assert!(tagged_heredoc_close_kind("WRONG", "TAG").is_none());
        assert!(tagged_heredoc_close_kind("TAGfoo", "TAG").is_none());
    }

    #[test]
    fn tagged_close_with_trailing_arg_on_same_line() {
        let line = r#"PLASM_INLINE_ARG, score=0, owner="inline-heredoc")"#;
        let (kind, close_ws) =
            tagged_heredoc_close_kind(line, "PLASM_INLINE_ARG").expect("close");
        assert_eq!(kind, HeredocCloseLineKind::GluedSuffix);
        assert_eq!(close_ws, 0);
        let close_at = close_ws + "PLASM_INLINE_ARG".len();
        assert_eq!(&line[close_at..], ", score=0, owner=\"inline-heredoc\")");
    }

    #[test]
    fn skip_tagged_errors_when_close_missing() {
        let s = "only body\nno sentinel";
        let err = skip_tagged_structured_heredoc(s, 0, "TAG").unwrap_err();
        assert!(err.contains("PLP-2:"), "{err}");
        assert!(err.contains("unterminated"), "{err}");
    }

    #[test]
    fn parse_tagged_heredoc_literal_extracts_body() {
        let raw = "<<PLASM_EOF\nhello\nworld\nPLASM_EOF";
        assert_eq!(parse_tagged_heredoc_literal(raw).unwrap(), "hello\nworld\n");
    }

    #[test]
    fn skip_tagged_closes_on_first_matching_line() {
        let s = "a\nTAG\n";
        let end = skip_tagged_structured_heredoc(s, 0, "TAG").expect("closed");
        assert!(end <= s.len());
    }
}
