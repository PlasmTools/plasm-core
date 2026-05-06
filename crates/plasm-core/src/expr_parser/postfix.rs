//! Postfix transforms on Plasm surface expressions (`.limit`, `.sort`, projections, etc.).
//!
//! This module **only** splits a string into a primary fragment and a left-to-right sequence of
//! postfix operations. It does not parse entity/query syntax — that remains [`super::parse`].
//!
//! **Bracket render** (`source[field,...] <<TAG … TAG`) is recognized by [`try_parse_bracket_render`]
//! using delimiter depth so `<<` inside calls/parens is not mistaken for a render opener.

/// Postfix operations peeled from the right of an expression, in **application order**
/// (index `0` applies first to the primary, then `1`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlasmPostfixOp {
    Singleton,
    PageSize(usize),
    Limit(usize),
    Sort { args: String },
    Aggregate { args: String },
    GroupBy { args: String },
    Projection { fields: String },
}

/// `source[field,...] <<TAG` … `TAG` render surface (program DAG lowering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BracketRender {
    pub source: String,
    /// Comma-separated field names from `[…]` (trimmed segments).
    pub fields: String,
    pub template: String,
}

/// Byte index of `[` matching the closing `]` at `close_bracket_idx` (nested brackets balanced).
pub(crate) fn matching_square_bracket_open(s: &str, close_bracket_idx: usize) -> Option<usize> {
    if close_bracket_idx >= s.len() || !s[close_bracket_idx..].starts_with(']') {
        return None;
    }
    let mut depth = 1i32;
    let mut quote = None::<char>;
    let prefix = &s[..close_bracket_idx];
    for (idx, c) in prefix.char_indices().rev() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            _ if quote.is_some() => {}
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

/// `author[login]` / nested chains → `author.login` for [`plasm_core::plasm_plan::FieldPath`].
pub fn normalize_nested_projection_field(segment: &str) -> Result<String, String> {
    let t = segment.trim();
    if !t.contains('[') {
        return Ok(t.to_string());
    }
    let close = t
        .rfind(']')
        .ok_or_else(|| format!("unbalanced `]` in projection `{segment}`"))?;
    let open = matching_square_bracket_open(t, close)
        .ok_or_else(|| format!("could not match `[` for trailing `]` in projection `{segment}`"))?;
    let inner_raw = &t[open + 1..close];
    let left = t[..open].trim_end();
    if left.is_empty() {
        return Err(format!(
            "nested projection `[…]` requires a left-hand identifier (`{segment}`)"
        ));
    }
    if left.contains('.') {
        return Err(format!(
            "nested projection `{segment}` must not use `.` to the left of `[…]`"
        ));
    }
    let inner_norm = normalize_nested_projection_field(inner_raw)?;
    Ok(format!("{left}.{inner_norm}"))
}

/// `ident[field,...]` with no trailing junk after `]` (same contract as program DAG `parse_projection`).
/// Closing line for row-to-text template heredocs — matches structured parameter heredocs
/// ([`crate::expr_parser::value`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderHeredocCloseLineKind {
    LineOnly,
    GluedSuffix,
}

fn render_tail_tagged_close_kind(
    line_slice: &str,
    tag: &str,
) -> Option<(RenderHeredocCloseLineKind, usize)> {
    let leading_ws = line_slice.len() - line_slice.trim_start().len();
    let t = line_slice.trim();
    if t == tag {
        return Some((RenderHeredocCloseLineKind::LineOnly, leading_ws));
    }
    if !t.starts_with(tag) {
        return None;
    }
    let after = &t[tag.len()..];
    let after = after.trim_start();
    if after.len() == 1 {
        let b = after.as_bytes()[0];
        if matches!(b, b')' | b',' | b'}') {
            return Some((RenderHeredocCloseLineKind::GluedSuffix, leading_ws));
        }
    }
    None
}

fn parse_projection_head(head: &str) -> Option<(&str, &str)> {
    let head = head.trim_end();
    if !head.ends_with(']') {
        return None;
    }
    let close = head.len() - 1;
    let open = matching_square_bracket_open(head, close)?;
    Some((head[..open].trim_end(), &head[open + 1..close]))
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
        if render_tail_tagged_close_kind(line_slice, tag).is_some() {
            return Ok(rest[body_start..line_start].to_string());
        }
        if pos >= bytes.len() {
            return Err(format!(
                "row-to-text template heredoc `<<{tag}` is not closed: after the template body, add a line whose trimmed text is `{tag}` (closes on the **first** such line; interior lines must not trim-equal `TAG`). Same discipline as structured `<<TAG` parameter heredocs."
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

/// Parsed `<<TAG … TAG` render tail after peeling postfix from the expression head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderTailParse {
    /// `source[field,...] <<TAG … TAG` — explicit columns for a row-to-text template.
    Explicit {
        source: String,
        fields: String,
        template: String,
    },
    /// `head <<TAG … TAG` where `head` has no trailing `[fields]` before `<<` — columns inferred at compile time.
    Inferred { head: String, template: String },
}

/// Split a program RHS into optional render tail parts at delimiter depth 0.
///
/// Chooses the **rightmost** `<<` at depth 0 whose heredoc closes cleanly.
pub fn try_parse_render_tail(rhs: &str) -> Result<Option<RenderTailParse>, String> {
    let mut positions: Vec<usize> = rhs.match_indices("<<").map(|(i, _)| i).collect();
    if positions.is_empty() {
        return Ok(None);
    }
    positions.sort_unstable_by_key(|&i| std::cmp::Reverse(i));

    for j in positions {
        if delimiter_depth_before(rhs, j) != 0 {
            continue;
        }
        let head = rhs[..j].trim_end();
        let rest = &rhs[j + 2..];

        if let Some((source, fields)) = parse_projection_head(head) {
            let template = parse_render_template_after_tag_opener(rest)?;
            return Ok(Some(RenderTailParse::Explicit {
                source: source.to_string(),
                fields: fields.to_string(),
                template,
            }));
        }

        let Ok(template) = parse_render_template_after_tag_opener(rest) else {
            continue;
        };
        // `<<TAG` at the start of the RHS is a tagged **value** heredoc (`body = <<TAG`), not a
        // row-to-text template (`binding <<TAG`). Require a non-empty expression head before `<<`.
        if head.trim().is_empty() {
            continue;
        }
        return Ok(Some(RenderTailParse::Inferred {
            head: head.to_string(),
            template,
        }));
    }
    Ok(None)
}

/// If `rhs` is `source[field,...] <<TAG` … `TAG` at **delimiter depth 0** (so `<<` inside `method(…)`
/// is not treated as render), return the parts. Otherwise `Ok(None)`.
///
/// Chooses the **rightmost** `<<` at depth 0 that yields a valid projection head and closed template.
pub fn try_parse_bracket_render(rhs: &str) -> Result<Option<BracketRender>, String> {
    Ok(match try_parse_render_tail(rhs)? {
        Some(RenderTailParse::Explicit {
            source,
            fields,
            template,
        }) => Some(BracketRender {
            source,
            fields,
            template,
        }),
        Some(RenderTailParse::Inferred { .. }) | None => None,
    })
}

/// Peel trailing postfix operators from `rhs`, returning `(primary, ops)`.
///
/// `ops` is ordered **inner → outer** (first apply `ops[0]` to `primary`, then `ops[1]`, …).
pub fn peel_postfix_suffixes(rhs: &str) -> Result<(String, Vec<PlasmPostfixOp>), String> {
    let mut cur = rhs.trim().to_string();
    let mut ops_rev: Vec<PlasmPostfixOp> = Vec::new();

    loop {
        let t = cur.trim();
        if t.is_empty() {
            return Err("empty expression after peeling postfix operators".into());
        }

        let mut progressed = false;

        if let Some(p) = strip_suffix_singleton(t) {
            ops_rev.push(PlasmPostfixOp::Singleton);
            cur = p;
            progressed = true;
        } else if let Some((p, n)) = strip_trailing_unary_int_call(t, "page_size")? {
            ops_rev.push(PlasmPostfixOp::PageSize(n));
            cur = p;
            progressed = true;
        } else if let Some((p, n)) = strip_trailing_unary_int_call(t, "limit")? {
            ops_rev.push(PlasmPostfixOp::Limit(n));
            cur = p;
            progressed = true;
        } else if let Some((p, args)) = strip_trailing_method_call(t, "sort")? {
            ops_rev.push(PlasmPostfixOp::Sort { args });
            cur = p;
            progressed = true;
        } else if let Some((p, args)) = strip_trailing_method_call(t, "aggregate")? {
            ops_rev.push(PlasmPostfixOp::Aggregate { args });
            cur = p;
            progressed = true;
        } else if let Some((p, args)) = strip_trailing_method_call(t, "group_by")? {
            ops_rev.push(PlasmPostfixOp::GroupBy { args });
            cur = p;
            progressed = true;
        } else if let Some((p, fields)) = strip_trailing_projection(t)? {
            ops_rev.push(PlasmPostfixOp::Projection { fields });
            cur = p;
            progressed = true;
        }

        if !progressed {
            break;
        }
    }

    let mut ops: Vec<PlasmPostfixOp> = ops_rev;
    ops.reverse();
    Ok((cur.trim().to_string(), ops))
}

fn strip_suffix_singleton(s: &str) -> Option<String> {
    let t = s.trim_end();
    let suf = ".singleton()";
    t.strip_suffix(suf).map(|p| p.trim_end().to_string())
}

/// Strips a trailing `.name(integer)` call at paren depth 0.
fn strip_trailing_unary_int_call(s: &str, name: &str) -> Result<Option<(String, usize)>, String> {
    let Some((prefix, args)) = strip_trailing_method_call(s, name)? else {
        return Ok(None);
    };
    let n = args
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{name}(...) requires a positive integer"))?;
    if n == 0 {
        return Err(format!("{name}(...) requires a positive integer"));
    }
    Ok(Some((prefix, n)))
}

/// Finds the **last** `.name(` at delimiter depth 0 whose closing `)` ends the string.
fn strip_trailing_method_call(s: &str, name: &str) -> Result<Option<(String, String)>, String> {
    let needle = format!(".{name}(");
    let mut search_end = s.len();
    while search_end > 0 {
        let slice = &s[..search_end];
        let Some(pos) = slice.rfind(&needle) else {
            return Ok(None);
        };
        if delimiter_depth_before(s, pos) != 0 {
            search_end = pos;
            continue;
        }
        let open_paren = pos + needle.len() - 1;
        let close = matching_paren_close(s, open_paren)?;
        if close + 1 != s.len() {
            search_end = pos;
            continue;
        }
        let args = s[open_paren + 1..close].to_string();
        return Ok(Some((s[..pos].trim_end().to_string(), args)));
    }
    Ok(None)
}

fn strip_trailing_projection(s: &str) -> Result<Option<(String, String)>, String> {
    let t = s.trim_end();
    if !t.ends_with(']') {
        return Ok(None);
    }
    let close = t.len() - 1;
    let Some(open) = matching_square_bracket_open(t, close) else {
        return Ok(None);
    };
    let fields = t[open + 1..close].to_string();
    Ok(Some((t[..open].trim_end().to_string(), fields)))
}

fn delimiter_depth_before(s: &str, end: usize) -> i32 {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (idx, c) in s.char_indices() {
        if idx >= end {
            break;
        }
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            _ if quote.is_some() => {}
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn matching_paren_close(s: &str, open_idx: usize) -> Result<usize, String> {
    let mut depth = 1i32;
    let mut quote = None::<char>;
    for (idx, c) in s.char_indices().skip(open_idx + 1) {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            _ if quote.is_some() => {}
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(idx);
                }
            }
            _ => {}
        }
    }
    Err("unbalanced parentheses in method call".into())
}

#[cfg(test)]
mod tests {
    //! Postfix **peel order** only (no full DAG). Full `.limit` / `.sort` semantics live in
    //! `plasm-e2e` `plasm_language_matrix` rows such as `lang_sort_limit`, `lang_limit_projection`,
    //! `lang_surface_line_limit`.
    use super::*;

    #[test]
    fn peel_limit_on_surface_primary() {
        let (p, ops) =
            peel_postfix_suffixes("e1{p4=e2(owner=\"x\", repo=\"y\")}.limit(20)").unwrap();
        assert_eq!(p, "e1{p4=e2(owner=\"x\", repo=\"y\")}");
        assert_eq!(ops, vec![PlasmPostfixOp::Limit(20)]);
    }

    /// Matrix analogue: `lang_sort_limit` / postfix ordering (peel vs planner lowering).
    #[test]
    fn peel_limit_then_sort() {
        let (p, ops) = peel_postfix_suffixes("a.limit(5).sort(x, desc)").unwrap();
        assert_eq!(p, "a");
        assert_eq!(
            ops,
            vec![
                PlasmPostfixOp::Limit(5),
                PlasmPostfixOp::Sort {
                    args: "x, desc".into()
                }
            ]
        );
    }

    #[test]
    fn peel_sort_then_limit() {
        let (p, ops) = peel_postfix_suffixes("a.sort(x).limit(3)").unwrap();
        assert_eq!(p, "a");
        assert_eq!(
            ops,
            vec![
                PlasmPostfixOp::Sort { args: "x".into() },
                PlasmPostfixOp::Limit(3),
            ]
        );
    }

    /// Matrix analogue: `lang_limit_projection` (projection token peel).
    #[test]
    fn peel_projection_and_limit() {
        let (p, ops) = peel_postfix_suffixes("e1{}.limit(10)[sha,message]").unwrap();
        assert_eq!(p, "e1{}");
        assert_eq!(
            ops,
            vec![
                PlasmPostfixOp::Limit(10),
                PlasmPostfixOp::Projection {
                    fields: "sha,message".into()
                },
            ]
        );
    }

    #[test]
    fn bracket_render_parses_at_depth_zero() {
        let r = try_parse_bracket_render("repo[p1,p2]<<T\nline one\nT").unwrap();
        let br = r.expect("bracket render");
        assert_eq!(br.source, "repo");
        assert_eq!(br.fields, "p1,p2");
        assert_eq!(br.template, "line one\n");
    }

    #[test]
    fn bracket_render_ignores_heredoc_inside_parens() {
        let r = try_parse_bracket_render("e3.m12(p76=<<M\nx\nM)").unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn bracket_render_err_when_projection_ok_but_heredoc_unclosed() {
        let e = try_parse_bracket_render("row[a,b]<<H\nno close").unwrap_err();
        assert!(
            e.contains("not closed") || e.contains("<<H"),
            "unexpected err: {e}"
        );
    }

    #[test]
    fn bracket_render_heredoc_first_tag_match_wins() {
        let br = try_parse_bracket_render("row[a]<<T\nhello\nT\nignored\nT")
            .unwrap()
            .unwrap();
        assert_eq!(br.template, "hello\n");
    }

    #[test]
    fn bracket_render_heredoc_glued_close_matches_structured_rules() {
        let br = try_parse_bracket_render("row[a]<<T\nx\nT)")
            .unwrap()
            .unwrap();
        assert_eq!(br.template, "x\n");
    }

    #[test]
    fn peel_projection_nested_brackets_on_primary() {
        let (p, ops) = peel_postfix_suffixes("e1().limit(10)[commits[author[login]],sha]").unwrap();
        assert_eq!(p, "e1()");
        assert_eq!(
            ops,
            vec![
                PlasmPostfixOp::Limit(10),
                PlasmPostfixOp::Projection {
                    fields: "commits[author[login]],sha".into()
                },
            ]
        );
    }

    #[test]
    fn normalize_nested_projection_bracket_sugar() {
        assert_eq!(
            normalize_nested_projection_field("author[login]").unwrap(),
            "author.login"
        );
        assert_eq!(
            normalize_nested_projection_field("commits[author[login]]").unwrap(),
            "commits.author.login"
        );
        assert_eq!(
            normalize_nested_projection_field("p91[p68]").unwrap(),
            "p91.p68"
        );
    }

    #[test]
    fn render_tail_inferred_binding_without_projection_bracket() {
        let r = try_parse_render_tail(
            "commits <<PLASM_RPT\n{% for r in rows %}{{ r.sha }}{% endfor %}\nPLASM_RPT",
        )
        .unwrap()
        .expect("render tail");
        match r {
            RenderTailParse::Inferred { head, template } => {
                assert_eq!(head, "commits");
                assert!(template.contains("for r in rows"));
            }
            RenderTailParse::Explicit { .. } => panic!("expected inferred"),
        }
    }

    #[test]
    fn render_tail_explicit_after_postfix_chain_before_heredoc() {
        let r = try_parse_render_tail("commits.limit(20)[sha,message] <<MD\none\nMD")
            .unwrap()
            .expect("render tail");
        match r {
            RenderTailParse::Explicit {
                source,
                fields,
                template,
            } => {
                assert_eq!(source, "commits.limit(20)");
                assert_eq!(fields, "sha,message");
                assert_eq!(template, "one\n");
            }
            RenderTailParse::Inferred { .. } => panic!("expected explicit"),
        }
    }
}
