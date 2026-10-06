//! Collect-meta peel (`.singleton` / `.page_size`) parsing.
//!
//! Does not parse entity/query syntax — that remains [`super::parse`].
//!
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CollectMetaError {
    #[error("unbalanced `]` in projection `{segment}`")]
    ProjectionCloseMissing { segment: String },
    #[error("could not match `[` for trailing `]` in projection `{segment}`")]
    ProjectionOpenMissing { segment: String },
    #[error("nested projection `[…]` requires a left-hand identifier (`{segment}`)")]
    ProjectionLeftRequired { segment: String },
    #[error("nested projection `{segment}` must not use `.` to the left of `[…]`")]
    ProjectionLeftDotted { segment: String },
    #[error("empty expression after peeling collect metadata")]
    EmptyExpression,
    #[error("legacy row algebra `.{method}` is not supported; use a `|` pipe stage")]
    LegacyMethod { method: String },
    #[error("legacy row projection `[…]` is not supported; use a `| select …` pipe stage")]
    LegacyProjection,
    #[error("legacy row projection `[{fields}]` is not supported; use `| select {fields}`")]
    LegacyProjectionFields { fields: String },
    #[error("{method}(...) requires a positive integer")]
    InvalidPositiveInteger {
        method: String,
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error(".{method} requires a positive integer, not zero")]
    ZeroInteger { method: String },
    #[error("expected `{{` while parsing a trailing brace block")]
    ExpectedBrace,
    #[error("unclosed `{{` in filter brace block")]
    UnclosedBrace,
    #[error("unbalanced parentheses in method call")]
    UnbalancedParentheses,
}

/// Collect metadata peeled from the right of an expression, in **application order**
/// (index `0` applies first to the primary, then `1`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectMeta {
    Singleton,
    PageSize(usize),
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
pub fn normalize_nested_projection_field(segment: &str) -> Result<String, CollectMetaError> {
    let t = segment.trim();
    if !t.contains('[') {
        return Ok(t.to_string());
    }
    let close = t
        .rfind(']')
        .ok_or_else(|| CollectMetaError::ProjectionCloseMissing {
            segment: segment.to_owned(),
        })?;
    let open = matching_square_bracket_open(t, close).ok_or_else(|| {
        CollectMetaError::ProjectionOpenMissing {
            segment: segment.to_owned(),
        }
    })?;
    let inner_raw = &t[open + 1..close];
    let left = t[..open].trim_end();
    if left.is_empty() {
        return Err(CollectMetaError::ProjectionLeftRequired {
            segment: segment.to_owned(),
        });
    }
    if left.contains('.') {
        return Err(CollectMetaError::ProjectionLeftDotted {
            segment: segment.to_owned(),
        });
    }
    let inner_norm = normalize_nested_projection_field(inner_raw)?;
    Ok(format!("{left}.{inner_norm}"))
}

/// Peel trailing collect metadata from `rhs`, returning `(primary, meta)`.
///
/// `meta` is ordered **inner → outer** (first apply `meta[0]` to `primary`, then `meta[1]`, …).
pub fn peel_collect_meta(rhs: &str) -> Result<(String, Vec<CollectMeta>), CollectMetaError> {
    reject_legacy_row_algebra(rhs)?;
    let mut cur = rhs.trim().to_string();
    let mut meta_rev: Vec<CollectMeta> = Vec::new();

    loop {
        let t = cur.trim();
        if t.is_empty() {
            return Err(CollectMetaError::EmptyExpression);
        }
        reject_legacy_row_algebra(t)?;

        let mut progressed = false;

        if let Some(p) = strip_suffix_singleton(t) {
            meta_rev.push(CollectMeta::Singleton);
            cur = p;
            progressed = true;
        } else if let Some((p, n)) = strip_trailing_unary_int_call(t, "page_size")? {
            meta_rev.push(CollectMeta::PageSize(n));
            cur = p;
            progressed = true;
        }

        if !progressed {
            break;
        }
    }

    let mut meta: Vec<CollectMeta> = meta_rev;
    meta.reverse();
    Ok((cur.trim().to_string(), meta))
}

fn reject_legacy_row_algebra(rhs: &str) -> Result<(), CollectMetaError> {
    const METHODS: &[&str] = &[
        "filter",
        "with",
        "group_by",
        "aggregate",
        "sort",
        "limit",
        "dedupe",
        "distinct",
    ];
    for method in METHODS {
        for opener in ['(', '{'] {
            let needle = format!(".{method}{opener}");
            if rhs
                .match_indices(&needle)
                .any(|(idx, _)| delimiter_depth_before(rhs, idx) == 0)
            {
                return Err(CollectMetaError::LegacyMethod {
                    method: (*method).to_owned(),
                });
            }
        }
        if strip_trailing_method_call(rhs, method)?.is_some()
            || strip_trailing_brace_block(rhs, method)?.is_some()
        {
            return Err(CollectMetaError::LegacyMethod {
                method: (*method).to_owned(),
            });
        }
    }
    if rhs
        .match_indices('[')
        .any(|(idx, _)| idx > 0 && delimiter_depth_before(rhs, idx) == 0)
    {
        return Err(CollectMetaError::LegacyProjection);
    }
    if let Some((source, fields)) = strip_trailing_projection(rhs)? {
        if !source.trim().is_empty() {
            return Err(CollectMetaError::LegacyProjectionFields { fields });
        }
    }
    Ok(())
}

fn strip_suffix_singleton(s: &str) -> Option<String> {
    let t = s.trim_end();
    let suf = ".singleton()";
    t.strip_suffix(suf).map(|p| p.trim_end().to_string())
}

/// Strips a trailing `.name(integer)` call at paren depth 0.
fn strip_trailing_unary_int_call(
    s: &str,
    name: &str,
) -> Result<Option<(String, usize)>, CollectMetaError> {
    let Some((prefix, args)) = strip_trailing_method_call(s, name)? else {
        return Ok(None);
    };
    let n = args.trim().parse::<usize>().map_err(|source| {
        CollectMetaError::InvalidPositiveInteger {
            method: name.to_owned(),
            value: args.trim().to_owned(),
            source,
        }
    })?;
    if n == 0 {
        return Err(CollectMetaError::ZeroInteger {
            method: name.to_owned(),
        });
    }
    Ok(Some((prefix, n)))
}

/// Finds the **last** `.name(` at delimiter depth 0 whose closing `)` ends the string.
/// `.name{preds}` at delimiter depth 0 with balanced `{…}`.
fn strip_trailing_brace_block(
    s: &str,
    name: &str,
) -> Result<Option<(String, String)>, CollectMetaError> {
    let needle = format!(".{name}{{");
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
        let open_brace = pos + needle.len() - 1;
        let close = matching_brace_close(s, open_brace)?;
        if close + 1 != s.len() {
            search_end = pos;
            continue;
        }
        let body = s[open_brace + 1..close].to_string();
        return Ok(Some((s[..pos].trim_end().to_string(), body)));
    }
    Ok(None)
}

fn matching_brace_close(s: &str, open: usize) -> Result<usize, CollectMetaError> {
    if !s[open..].starts_with('{') {
        return Err(CollectMetaError::ExpectedBrace);
    }
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (idx, c) in s[open..].char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            _ if quote.is_some() => {}
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(open + idx);
                }
            }
            _ => {}
        }
    }
    Err(CollectMetaError::UnclosedBrace)
}

fn strip_trailing_method_call(
    s: &str,
    name: &str,
) -> Result<Option<(String, String)>, CollectMetaError> {
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

fn strip_trailing_projection(s: &str) -> Result<Option<(String, String)>, CollectMetaError> {
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

fn matching_paren_close(s: &str, open_idx: usize) -> Result<usize, CollectMetaError> {
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
    Err(CollectMetaError::UnbalancedParentheses)
}

#[cfg(test)]
mod tests {
    //! Collect-meta peel + legacy reject + `=> <<` render opener.
    use super::*;

    #[test]
    fn peel_page_size_and_singleton() {
        let (p, ops) = peel_collect_meta("rows.page_size(50).singleton()").unwrap();
        assert_eq!(p, "rows");
        assert_eq!(ops, vec![CollectMeta::PageSize(50), CollectMeta::Singleton]);
    }

    #[test]
    fn rejects_legacy_limit_filter_with_projection() {
        for src in [
            "e1{}.limit(20)",
            "a.filter{x=1}",
            "a.filter(x=1)",
            "a.with{y: 1}",
            "a.with(y=1)",
            "a.sort(x)",
            "a.group_by(k, n=count)",
            "a.aggregate(n=count)",
            "a.dedupe(k)",
            "a.distinct()",
            "e1{}[sha,message]",
        ] {
            let err = peel_collect_meta(src).expect_err(src);
            assert!(
                matches!(
                    err,
                    CollectMetaError::LegacyMethod { .. }
                        | CollectMetaError::LegacyProjection
                        | CollectMetaError::LegacyProjectionFields { .. }
                ),
                "src={src} err={err}"
            );
        }
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
    }

    #[test]
    fn peel_does_not_treat_join_or_open_as_row_compute() {
        let (p, ops) = peel_collect_meta("issues.join(comments)").unwrap();
        assert!(ops.is_empty(), "join is not a postfix verb, got {ops:?}");
        assert!(p.contains("join"));
        let (p2, ops2) = peel_collect_meta("issues.open(labels)").unwrap();
        assert!(ops2.is_empty(), "open is not a postfix verb, got {ops2:?}");
        assert!(p2.contains("open"));
    }
}
