//! State iterator surface: `iterate seed step mut until pred take N` (PLP-8).

use super::applicator::method_call_at_depth_zero;
use super::program_surface::is_valid_program_label;

/// Parsed `iterate … step … until … take N` (hard bound required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterateUntilExpr {
    /// Seed observe surface (Get / singleton binding / proven singleton primary).
    pub seed: String,
    /// Write/side-effect invoke surface; `_` is the current cursor row.
    pub step: String,
    /// Stop predicate body (same atom shape as `| where`, optional `_.` field prefix).
    pub until: String,
    /// Mandatory max step executions (`N ≥ 1`).
    pub take: u32,
}

/// Try parse a full expression as state iterate. `Ok(None)` if not an `iterate` form.
pub fn try_parse_iterate_until(raw: &str) -> Result<Option<IterateUntilExpr>, String> {
    let t = raw.trim();
    if t.starts_with("while")
        && (t.len() == 5 || t.as_bytes().get(5).is_some_and(|b| b.is_ascii_whitespace()))
    {
        return Err(iterate_usage_err(
            "unbounded `while` is forbidden; use `iterate … step … until … take N` (hard bound required)",
        ));
    }
    if !t.starts_with("iterate") {
        return Ok(None);
    }
    let after_kw = &t["iterate".len()..];
    if !after_kw.is_empty() && !after_kw.starts_with(|c: char| c.is_ascii_whitespace()) {
        return Ok(None);
    }
    let rest = after_kw.trim_start();
    if rest.is_empty() {
        return Err(iterate_usage_err(
            "missing seed after `iterate` (expected `iterate seed step … until … take N`)",
        ));
    }

    let (seed, after_seed) = split_keyword_clause(rest, "step")?;
    let seed = seed.trim();
    if seed.is_empty() {
        return Err(iterate_usage_err("seed expression is empty"));
    }

    let (step, after_step) = split_keyword_clause(after_seed, "until")?;
    let step = step.trim();
    if step.is_empty() {
        return Err(iterate_usage_err("step expression is empty"));
    }
    if !method_call_at_depth_zero(step) {
        return Err(iterate_usage_err(
            "step must be a write/side-effect invoke (`Entity.m#(…)` / `Entity.method(…)`)",
        ));
    }

    let (until, after_until) = split_keyword_clause(after_step, "take")?;
    let until = normalize_until_pred(until.trim());
    if until.is_empty() {
        return Err(iterate_usage_err("until predicate is empty"));
    }

    let take_raw = after_until.trim();
    if take_raw.is_empty() {
        return Err(iterate_usage_err(
            "`take N` is required (hard bound; no unbounded iterate)",
        ));
    }
    if take_raw.contains(char::is_whitespace) || take_raw.contains(|c: char| !c.is_ascii_digit()) {
        return Err(iterate_usage_err(&format!(
            "`take` requires a positive integer bound, got `{take_raw}`"
        )));
    }
    let take: u32 = take_raw
        .parse()
        .map_err(|_| iterate_usage_err(&format!("invalid take bound `{take_raw}`")))?;
    if take == 0 {
        return Err(iterate_usage_err("`take N` requires N ≥ 1"));
    }

    // Reject seed that looks like a domain symbol alone without call/braces when it's not a label.
    // Labels and Entity(…) / Entity{…} are fine; bare `e1` is a label-or-entity head (ok).
    if seed.contains("=>") {
        return Err(iterate_usage_err(
            "seed must not include `=>` applicators; bind the seed first",
        ));
    }

    Ok(Some(IterateUntilExpr {
        seed: seed.to_string(),
        step: step.to_string(),
        until,
        take,
    }))
}

fn normalize_until_pred(raw: &str) -> String {
    let t = raw.trim();
    // Allow `_.field = …` as sugar for `field = …` (cursor is implicit).
    if let Some(rest) = t.strip_prefix("_.") {
        rest.trim().to_string()
    } else {
        t.to_string()
    }
}

fn iterate_usage_err(detail: &str) -> String {
    format!(
        "invalid state iterate: {detail}; lawful form: `iterate seed step Entity.m#(…) until field = value take N` (PLP-8; `take N` mandatory)"
    )
}

/// Split `head KEYWORD tail` at depth-0 keyword token.
fn split_keyword_clause<'a>(src: &'a str, keyword: &str) -> Result<(&'a str, &'a str), String> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<u8>;
    let kw = keyword.as_bytes();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' if quote == Some(b) => quote = None,
            b'"' | b'\'' if quote.is_none() => quote = Some(b),
            b'(' | b'[' | b'{' if quote.is_none() => depth += 1,
            b')' | b']' | b'}' if quote.is_none() => depth -= 1,
            _ if quote.is_none() && depth == 0 && is_keyword_at(bytes, i, kw) => {
                let before = src[..i].trim_end();
                let after = src[i + kw.len()..].trim_start();
                return Ok((before, after));
            }
            _ => {}
        }
        i += 1;
    }
    Err(iterate_usage_err(&format!(
        "missing `{keyword}` clause (hard-bound iterate requires step, until, and take)"
    )))
}

fn is_keyword_at(bytes: &[u8], i: usize, kw: &[u8]) -> bool {
    if i + kw.len() > bytes.len() {
        return false;
    }
    if i > 0 {
        let prev = bytes[i - 1];
        if !prev.is_ascii_whitespace() {
            return false;
        }
    }
    if !bytes[i..].starts_with(kw) {
        return false;
    }
    let after = i + kw.len();
    if after < bytes.len() {
        let n = bytes[after];
        if !n.is_ascii_whitespace() {
            return false;
        }
    }
    true
}

/// True when `seed` is a single program label (binding reuse).
pub fn iterate_seed_is_label(seed: &str) -> bool {
    let t = seed.trim();
    is_valid_program_label(t)
        && !t.contains('(')
        && !t.contains('{')
        && !t.contains('.')
        && !t.contains('|')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_iterate() {
        let got = try_parse_iterate_until(
            r#"iterate LangCursor("c1") step LangCursor(_.id).tick() until phase = "done" take 8"#,
        )
        .expect("parse")
        .expect("iterate");
        assert_eq!(got.seed, r#"LangCursor("c1")"#);
        assert_eq!(got.step, "LangCursor(_.id).tick()");
        assert_eq!(got.until, r#"phase = "done""#);
        assert_eq!(got.take, 8);
    }

    #[test]
    fn strips_underscore_field_prefix() {
        let got = try_parse_iterate_until(
            r#"iterate cur step LangItem(_.id).update(score=11, title=_.title, owner=_.owner) until _.score = 11 take 3"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got.until, "score = 11");
    }

    #[test]
    fn rejects_missing_take() {
        let err = try_parse_iterate_until(
            r#"iterate LangItem("i1") step LangItem(_.id).ping() until active = true"#,
        )
        .expect_err("take required");
        assert!(err.contains("take"), "{err}");
    }

    #[test]
    fn rejects_take_zero() {
        let err = try_parse_iterate_until(
            r#"iterate LangItem("i1") step LangItem(_.id).ping() until active = true take 0"#,
        )
        .expect_err("N>=1");
        assert!(err.contains("N ≥ 1") || err.contains("N >= 1"), "{err}");
    }

    #[test]
    fn non_iterate_returns_none() {
        assert!(try_parse_iterate_until(r#"LangItem("i1")"#)
            .unwrap()
            .is_none());
        assert!(
            try_parse_iterate_until("items => LangItem(_.id).update(score=1)")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_while() {
        let err = try_parse_iterate_until("while true").expect_err("while");
        assert!(err.contains("while") && err.contains("iterate"), "{err}");
    }
}
