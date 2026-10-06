//! State iterator surface: Get-identity seed (literal or bound) then
//! `iterate cur step mut until pred take N` (PLP-8).

use super::applicator::method_call_at_depth_zero;
use super::program_surface::is_valid_program_label;
use thiserror::Error;

const ITERATE_FORM: &str = "`cur = e#(\"id\")` / `cur = e#(tok)` / `cur = e#{id_field=tok}` then `iterate cur step Entity.m#(…) until field = value take N` (PLP-8; `take N` mandatory)";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IterateUntilError {
    #[error("invalid state iterate: unbounded `while` is forbidden; use `iterate … step … until … take N` (hard bound required); lawful form: {ITERATE_FORM}")]
    UnboundedWhile,
    #[error("invalid state iterate: missing seed after `iterate` (expected a Get identity: `cur = e#(\"id\")` / `cur = e#(tok)` / `cur = e#{{id_field=tok}}` then `iterate cur step … until … take N`); lawful form: {ITERATE_FORM}")]
    MissingSeed,
    #[error("invalid state iterate: seed expression is empty; lawful form: {ITERATE_FORM}")]
    EmptySeed,
    #[error("invalid state iterate: missing `step` clause (hard-bound iterate requires step, until, and take); lawful form: {ITERATE_FORM}")]
    MissingStepClause,
    #[error("invalid state iterate: step expression is empty; lawful form: {ITERATE_FORM}")]
    EmptyStep,
    #[error("invalid state iterate: step is a keyword followed by an invoke (`iterate cur step Entity.m#(…)`), not a binder; lawful form: {ITERATE_FORM}")]
    StepIsBinder,
    #[error("invalid state iterate: step must be a write/side-effect invoke (`Entity.m#(…)` / `Entity.method(…)`); lawful form: {ITERATE_FORM}")]
    InvalidStep,
    #[error("invalid state iterate: missing `until` clause (hard-bound iterate requires step, until, and take); lawful form: {ITERATE_FORM}")]
    MissingUntilClause,
    #[error("invalid state iterate: until predicate is empty; lawful form: {ITERATE_FORM}")]
    EmptyUntil,
    #[error("invalid state iterate: missing `take N` bound (hard bound; no unbounded iterate); lawful form: {ITERATE_FORM}")]
    MissingTake,
    #[error("invalid state iterate: `take` requires a positive integer bound, got `{value}`; lawful form: {ITERATE_FORM}")]
    InvalidTake { value: String },
    #[error("invalid state iterate: invalid take bound `{value}`; lawful form: {ITERATE_FORM}")]
    TakeOutOfRange { value: String },
    #[error("invalid state iterate: `take N` requires N ≥ 1; lawful form: {ITERATE_FORM}")]
    ZeroTake,
    #[error("invalid state iterate: seed must not include `=>` applicators; bind the seed first; lawful form: {ITERATE_FORM}")]
    SeedApplicator,
    #[error("iterate seed `{seed}` must be a catalog Get identity ({ITERATE_SEED_GET_FAMILY}) so the seed can be re-observed")]
    SeedMustBeGetIdentity { seed: String },
}

/// Parsed `iterate … step … until … take N` (hard bound required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterateUntilExpr {
    /// Seed observe surface: catalog Get identity, or a binding of one.
    pub seed: String,
    /// Write/side-effect invoke surface; `_` is the current cursor row.
    pub step: String,
    /// Stop predicate body (same atom shape as `| where`, optional `_.` field prefix).
    pub until: String,
    /// Mandatory max step executions (`N ≥ 1`).
    pub take: u32,
}

/// Try parse a full expression as state iterate. `Ok(None)` if not an `iterate` form.
pub fn try_parse_iterate_until(raw: &str) -> Result<Option<IterateUntilExpr>, IterateUntilError> {
    let t = raw.trim();
    if t.starts_with("while")
        && (t.len() == 5 || t.as_bytes().get(5).is_some_and(|b| b.is_ascii_whitespace()))
    {
        return Err(IterateUntilError::UnboundedWhile);
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
        return Err(IterateUntilError::MissingSeed);
    }

    let (seed, after_seed) = split_keyword_clause(rest, "step")?;
    let seed = seed.trim();
    if seed.is_empty() {
        return Err(IterateUntilError::EmptySeed);
    }

    let (step, after_step) = split_keyword_clause(after_seed, "until")?;
    let step = step.trim();
    if step.is_empty() {
        return Err(IterateUntilError::EmptyStep);
    }
    if step.starts_with('=') {
        return Err(IterateUntilError::StepIsBinder);
    }
    if !method_call_at_depth_zero(step) {
        return Err(IterateUntilError::InvalidStep);
    }

    let (until, after_until) = split_keyword_clause(after_step, "take")?;
    let until = normalize_until_pred(until.trim());
    if until.is_empty() {
        return Err(IterateUntilError::EmptyUntil);
    }

    let take_raw = after_until.trim();
    if take_raw.is_empty() {
        return Err(IterateUntilError::MissingTake);
    }
    if take_raw.contains(char::is_whitespace) || take_raw.contains(|c: char| !c.is_ascii_digit()) {
        return Err(IterateUntilError::InvalidTake {
            value: take_raw.to_owned(),
        });
    }
    let take: u32 = take_raw
        .parse()
        .map_err(|_| IterateUntilError::TakeOutOfRange {
            value: take_raw.to_owned(),
        })?;
    if take == 0 {
        return Err(IterateUntilError::ZeroTake);
    }

    // Reject seed that looks like a domain symbol alone without call/braces when it's not a label.
    // Labels and Entity(…) / Entity{…} are fine; bare `e1` is a label-or-entity head (ok).
    if seed.contains("=>") {
        return Err(IterateUntilError::SeedApplicator);
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

/// Taught Get-identity seed family (literal or bound). Re-observe replays `seed_ir`.
pub const ITERATE_SEED_GET_FAMILY: &str =
    "`cur = e#(\"id\")` / `cur = e#(tok)` / `cur = e#{id_field=tok}` then `iterate cur step …`";

/// Taught repair when iterate seed is not a catalog Get identity (PLP-8).
pub fn iterate_seed_must_be_get_identity(seed: &str) -> IterateUntilError {
    IterateUntilError::SeedMustBeGetIdentity {
        seed: seed.to_owned(),
    }
}

/// Split `head KEYWORD tail` at depth-0 keyword token.
fn split_keyword_clause<'a>(
    src: &'a str,
    keyword: &str,
) -> Result<(&'a str, &'a str), IterateUntilError> {
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
    Err(match keyword {
        "step" => IterateUntilError::MissingStepClause,
        "until" => IterateUntilError::MissingUntilClause,
        _ => IterateUntilError::MissingTake,
    })
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
        assert!(matches!(err, IterateUntilError::MissingTake));
    }

    #[test]
    fn rejects_take_zero() {
        let err = try_parse_iterate_until(
            r#"iterate LangItem("i1") step LangItem(_.id).ping() until active = true take 0"#,
        )
        .expect_err("N>=1");
        assert!(matches!(err, IterateUntilError::ZeroTake));
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
        assert!(matches!(err, IterateUntilError::UnboundedWhile));
    }

    #[test]
    fn seed_get_identity_diagnostic_names_taught_form() {
        let err = iterate_seed_must_be_get_identity("cur");
        assert!(matches!(
            err,
            IterateUntilError::SeedMustBeGetIdentity { .. }
        ));
        let rendered = err.to_string();
        assert!(rendered.contains("cur = e#(\"id\")"), "{rendered}");
        assert!(rendered.contains("e#(tok)"), "{rendered}");
        assert!(rendered.contains("e#{id_field=tok}"), "{rendered}");
        assert!(rendered.contains("iterate cur step"), "{rendered}");
        assert!(!rendered.contains("carry ir"), "{rendered}");
    }

    #[test]
    fn rejects_step_equals_as_binder() {
        let err = try_parse_iterate_until(
            r#"iterate cur step = LangCursor(_.id).tick() until phase = "done" take 4"#,
        )
        .expect_err("step is a keyword; `step = invoke` is not lawful");
        assert!(
            err.to_string().contains("not a binder")
                && err.to_string().contains("iterate cur step Entity.m#(…)"),
            "diagnostic must name the executable invoke-after-step form, got: {err}"
        );
    }
}
