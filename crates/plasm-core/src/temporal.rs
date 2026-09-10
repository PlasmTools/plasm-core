//! Temporal parsing and wire encoding for Plasm.
//!
//! **Predicate / expression input** — [`normalize_temporal_value`]: values typed as
//! [`FieldType::Date`] in path expressions and predicates (see [`crate::expr_parser`]). Bare `now`
//! resolves to the reference instant ([`temporal_reference_now`]: host UTC unless
//! `PLASM_TEMPORAL_NOW` is set).
//!
//! **Eval override:** set `PLASM_TEMPORAL_NOW` to an RFC3339 or `YYYY-MM-DDTHH:MM:SS` UTC timestamp
//! so relative phrases (`now`, `today`, `7 days ago`, …) anchor to a harness world clock (e.g.
//! AppWorld task `specs.json` `datetime` on an out-of-process `plasm-mcp`).
//!
//! **URL / query wire slots** — [`wire_temporal_value`]: preserves backend-relative literals
//! (`now`, `now-1h`, …) and all-digit opaque tokens; otherwise same NL/ISO parsing as predicates.
//!
//! **Not used for:** decoding JSON responses, cache rows, REPL tables, summaries, or any
//! **display** path — those show API values as returned by the backend.
//!
//! Uses [`chrono_english::parse_date_string`] (GNU-`date`-style English; see
//! [chrono-english](https://docs.rs/chrono-english)) for forgiving natural-language and ISO-ish
//! inputs after the fixed phrase table below. Bare integer/float tokens are interpreted as Unix
//! **seconds** (≤10¹²) or **milliseconds** (≥10¹²) before formatting. The string **`now`**
//! (case-insensitive) resolves to the reference instant — [`parse_date_string`] does not
//! treat `now` as a special token.
//!
//! **Pre-normalisation:** strings with **no ASCII digits** have `-` replaced with spaces so
//! `next-week` becomes `next week` before [`parse_date_string`] and before fixed relative-phrase
//! resolution. (Hyphenated ISO dates and timestamps keep their `-` because they contain digits.)
//!
//! **Fixed relative English phrases** (`today`, `next week`, `this year`, …) are resolved here
//! with chrono for stable semantics; everything else goes to `chrono-english` with
//! [`Dialect::Uk`](chrono_english::Dialect).

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_english::{parse_date_string, Dialect};
use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

use crate::{TemporalWireFormat, Value};

/// Compact English unit for chrono-english (`7d ago`, `in 3 hours`).
fn expand_unit_word(unit: &str) -> &'static str {
    match unit.to_ascii_lowercase().as_str() {
        "d" | "day" | "days" => "days",
        "h" | "hr" | "hrs" | "hour" | "hours" => "hours",
        "m" | "min" | "mins" | "minute" | "minutes" => "minutes",
        "w" | "week" | "weeks" => "weeks",
        "s" | "sec" | "secs" | "second" | "seconds" => "seconds",
        _ => "days",
    }
}

fn compact_unit(unit: &str) -> &'static str {
    match expand_unit_word(unit) {
        "days" => "d",
        "hours" => "h",
        "minutes" => "m",
        "weeks" => "w",
        "seconds" => "s",
        _ => "d",
    }
}

/// Quote multi-word temporal phrases so `| where` / brace parsers treat them as one value.
fn quote_if_spaced(s: String) -> String {
    if s.chars().any(char::is_whitespace) && !s.starts_with('"') && !s.starts_with('\'') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

fn map_temporal_replacement(raw: String, quote_spaced: bool) -> String {
    if quote_spaced {
        quote_if_spaced(raw)
    } else {
        raw
    }
}

/// Rewrite LLM / Kusto / wire-shaped temporal **literals and subexpressions** into
/// forms accepted by [`normalize_temporal_value`] (chrono-english + fixed phrases).
///
/// Does **not** change wire-opaque semantics in [`wire_temporal_value`] — call this only on
/// predicate / Date-coerce / `| where` surfaces.
///
/// Examples (non-exhaustive):
/// - `now-7d` → `7d ago`
/// - `now() - 7d` / `now()-7d` → `7d ago`
/// - `datetime(now()) - duration(7, "day")` → `7 days ago`
/// - bare `now()` → `now`
pub fn rewrite_temporal_aliases(input: &str) -> Cow<'_, str> {
    rewrite_temporal_aliases_inner(input, false)
}

/// Like [`rewrite_temporal_aliases`], but quotes multi-word replacements so they survive
/// brace / `| where` value tokenization (`7d ago` → `"7d ago"`).
pub fn rewrite_temporal_aliases_in_predicate_body(input: &str) -> Cow<'_, str> {
    rewrite_temporal_aliases_inner(input, true)
}

fn rewrite_temporal_aliases_inner(input: &str, quote_spaced: bool) -> Cow<'_, str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Cow::Borrowed(input);
    }
    let mut out = trimmed.to_string();
    let mut changed = false;
    for _ in 0..8 {
        let next = rewrite_temporal_aliases_once(&out, quote_spaced);
        if next == out {
            break;
        }
        out = next;
        changed = true;
    }
    if changed || out != trimmed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(input)
    }
}

fn rewrite_temporal_aliases_once(s: &str, quote_spaced: bool) -> String {
    static RE_KUSTO_DURATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?ix)
            (?:datetime\s*\(\s*)?now\s*\(\s*\)\s*\)?\s*
            -\s*
            duration\s*\(\s*(\d+)\s*,\s*['"]([a-z]+)['"]\s*\)
            "#,
        )
        .expect("kusto duration regex")
    });
    static RE_NOW_CALL_MINUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)
            (?:datetime\s*\(\s*)?now\s*\(\s*\)\s*\)?\s*
            -\s*
            (\d+)\s*([a-z]+)
            ",
        )
        .expect("now() minus regex")
    });
    static RE_NOW_CALL_PLUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)
            (?:datetime\s*\(\s*)?now\s*\(\s*\)\s*\)?\s*
            \+\s*
            (\d+)\s*([a-z]+)
            ",
        )
        .expect("now() plus regex")
    });
    static RE_NOW_HYPHEN_OFFSET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)\bnow\s*-\s*(\d+)\s*([dhmsw]|days?|hours?|hrs?|minutes?|mins?|weeks?|seconds?|secs?)\b",
        )
        .expect("now-Nd regex")
    });
    static RE_NOW_PLUS_OFFSET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)\bnow\s*\+\s*(\d+)\s*([dhmsw]|days?|hours?|hrs?|minutes?|mins?|weeks?|seconds?|secs?)\b",
        )
        .expect("now+Nd regex")
    });
    static RE_START_OF_DAY_TODAY_MINUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?ix)start_of_day\s*\(\s*today\s*-\s*(\d+)\s*(?:days?|d)\s*\)")
            .expect("start_of_day(today-N) regex")
    });
    static RE_TODAY_MINUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?ix)\btoday\s*-\s*(\d+)\s*(?:days?|d)\b").expect("today-N days regex")
    });
    static RE_DATETIME_NOW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?ix)\bdatetime\s*\(\s*now\s*\(\s*\)\s*\)").expect("datetime(now()) regex")
    });
    static RE_BARE_NOW_CALL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bnow\s*\(\s*\)").expect("bare now() regex"));
    static RE_AGO_FUNC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?ix)\bago\s*\(\s*(\d+)\s*([a-z]+)\s*\)").expect("ago() regex")
    });

    let q = quote_spaced;
    let mut out = RE_KUSTO_DURATION
        .replace_all(s, |caps: &regex::Captures| {
            let n = &caps[1];
            let word = expand_unit_word(&caps[2]);
            map_temporal_replacement(format!("{n} {word} ago"), q)
        })
        .into_owned();
    out = RE_NOW_CALL_MINUS
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            let u = compact_unit(&caps[2]);
            map_temporal_replacement(format!("{n}{u} ago"), q)
        })
        .into_owned();
    out = RE_NOW_CALL_PLUS
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            let word = expand_unit_word(&caps[2]);
            map_temporal_replacement(format!("in {n} {word}"), q)
        })
        .into_owned();
    out = RE_NOW_HYPHEN_OFFSET
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            let u = compact_unit(&caps[2]);
            map_temporal_replacement(format!("{n}{u} ago"), q)
        })
        .into_owned();
    out = RE_NOW_PLUS_OFFSET
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            let word = expand_unit_word(&caps[2]);
            map_temporal_replacement(format!("in {n} {word}"), q)
        })
        .into_owned();
    out = RE_START_OF_DAY_TODAY_MINUS
        .replace_all(&out, |caps: &regex::Captures| {
            map_temporal_replacement(format!("{} days ago", &caps[1]), q)
        })
        .into_owned();
    out = RE_TODAY_MINUS
        .replace_all(&out, |caps: &regex::Captures| {
            map_temporal_replacement(format!("{} days ago", &caps[1]), q)
        })
        .into_owned();
    out = RE_AGO_FUNC
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            let u = compact_unit(&caps[2]);
            map_temporal_replacement(format!("{n}{u} ago"), q)
        })
        .into_owned();
    out = RE_DATETIME_NOW.replace_all(&out, "now").into_owned();
    out = RE_BARE_NOW_CALL.replace_all(&out, "now").into_owned();
    out
}

/// Human-readable alias list for parse / type feedback (predicate Date slots).
pub fn temporal_predicate_alias_hint() -> &'static str {
    "For date/time **predicate** / `| where` values use normalize-legal aliases: \
     `7d ago`, `7 days ago`, `in 3 hours`, `today`, `yesterday` (also `1d ago` / \
     `0d ago` as calendar-day midnights), `last week`, `last monday`, RFC3339 \
     (`2024-06-01T12:00:00Z`), or Unix ms — not `now()`, `now() - 7d`, or wire-only \
     `now-7d` (those are rewritten when possible; prefer the English forms). \
     \"Last N days (including today)\" → `Nd ago` / `N days ago` (inclusive; not N−1)."
}

fn datetime_from_integer(i: i64) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if i.abs() >= 1_000_000_000_000 {
        chrono::Utc
            .timestamp_millis_opt(i)
            .single()
            .ok_or_else(|| format!("timestamp millis out of range: {i}"))
    } else {
        chrono::Utc
            .timestamp_opt(i, 0)
            .single()
            .ok_or_else(|| format!("timestamp seconds out of range: {i}"))
    }
}

fn datetime_from_float(f: f64) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let ms = (f * 1000.0).round() as i64;
    datetime_from_integer(ms)
}

/// If the token has no ASCII digits, treat `-` as a word separator (`next-week` → `next week`).
/// ISO dates and Unix-looking strings keep their hyphens.
fn normalize_natural_language_temporal_input(s: &str) -> String {
    if s.chars().any(|c| c.is_ascii_digit()) {
        s.to_string()
    } else {
        s.replace('-', " ")
    }
}

fn utc_midnight(d: NaiveDate) -> chrono::DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("valid midnight").and_utc()
}

/// Parse `PLASM_TEMPORAL_NOW` / harness override strings (RFC3339, naive UTC, or Unix digits).
pub fn parse_temporal_now_env(raw: &str) -> Result<DateTime<Utc>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("PLASM_TEMPORAL_NOW must not be empty".into());
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(ndt.and_utc());
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        let n: i64 = s
            .parse()
            .map_err(|_| format!("PLASM_TEMPORAL_NOW: invalid integer timestamp `{s}`"))?;
        return datetime_from_integer(n);
    }
    Err(format!(
        "PLASM_TEMPORAL_NOW: invalid datetime `{s}` (expected RFC3339, YYYY-MM-DDTHH:MM:SS UTC, or Unix digits)"
    ))
}

/// Reference instant for relative temporal resolution (`now`, `today`, `7 days ago`, …).
///
/// Uses `PLASM_TEMPORAL_NOW` when set; otherwise [`Utc::now()`].
pub fn temporal_reference_now() -> Result<DateTime<Utc>, String> {
    match std::env::var("PLASM_TEMPORAL_NOW") {
        Ok(raw) if raw.trim().is_empty() => Ok(Utc::now()),
        Ok(raw) => parse_temporal_now_env(&raw),
        Err(_) => Ok(Utc::now()),
    }
}

/// Compact `0d ago` / `1d ago` (and `N days ago`) as **calendar-day** anchors.
///
/// Models often emit these for “today” / “yesterday”. Without this map, chrono-english
/// treats them as rolling instants (`now − Nd`), which disagrees with `today` /
/// `yesterday` midnight semantics. Only **0** and **1** day offsets are remapped;
/// `2d ago` and longer stay rolling via chrono-english.
fn resolve_compact_calendar_day_offset(
    lowercase_ascii: &str,
    reference: DateTime<Utc>,
) -> Option<chrono::DateTime<Utc>> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^([01])\s*(?:d|days?)\s+ago$").expect("0/1d ago calendar regex")
    });
    let caps = RE.captures(lowercase_ascii.trim())?;
    let n: i64 = caps.get(1)?.as_str().parse().ok()?;
    let today = reference.date_naive();
    match n {
        0 => Some(utc_midnight(today)),
        1 => Some(utc_midnight(today - Duration::days(1))),
        _ => None,
    }
}

/// Common relative phrases for predicate input. Times are **midnight UTC** on the resolved
/// calendar day, except `now` (handled above) which uses the reference instant.
fn resolve_relative_english_phrase(
    lowercase_ascii: &str,
    reference: DateTime<Utc>,
) -> Option<chrono::DateTime<Utc>> {
    let today = reference.date_naive();
    match lowercase_ascii.trim() {
        "today" => Some(utc_midnight(today)),
        "tomorrow" => Some(utc_midnight(today + Duration::days(1))),
        "yesterday" => Some(utc_midnight(today - Duration::days(1))),
        // Calendar week offset from today (not ISO week boundary).
        "next week" => Some(utc_midnight(today + Duration::weeks(1))),
        "last week" => Some(utc_midnight(today - Duration::weeks(1))),
        "next month" => today.checked_add_months(Months::new(1)).map(utc_midnight),
        "last month" => today.checked_sub_months(Months::new(1)).map(utc_midnight),
        // Same rolling convention as months: ±12 months from today's calendar date.
        "next year" => today.checked_add_months(Months::new(12)).map(utc_midnight),
        "last year" => today.checked_sub_months(Months::new(12)).map(utc_midnight),
        // Start of the current calendar year (Jan 1 UTC).
        "this year" => NaiveDate::from_ymd_opt(today.year(), 1, 1).map(utc_midnight),
        other => resolve_compact_calendar_day_offset(other, reference),
    }
}

fn parse_to_utc_at_reference(
    val: &Value,
    reference: DateTime<Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, String> {
    match val {
        Value::String(s) => {
            let rewritten = rewrite_temporal_aliases(s);
            let t = rewritten.trim();
            if t.is_empty() {
                return Err("empty date string".to_string());
            }
            if t.eq_ignore_ascii_case("now") {
                return Ok(reference);
            }
            if t.chars().all(|c| c.is_ascii_digit()) {
                let n: i64 = t
                    .parse()
                    .map_err(|_| format!("invalid integer timestamp: {t}"))?;
                return datetime_from_integer(n);
            }
            let normalized = normalize_natural_language_temporal_input(t);
            let lower = normalized.to_ascii_lowercase();
            if let Some(dt) = resolve_relative_english_phrase(&lower, reference) {
                return Ok(dt);
            }
            parse_date_string(&normalized, reference, Dialect::Uk).map_err(|e| e.to_string())
        }
        Value::Integer(i) => datetime_from_integer(*i),
        Value::Float(f) => datetime_from_float(*f),
        _ => Err(format!("cannot interpret {} as date/time", val.type_name())),
    }
}

fn parse_to_utc(val: &Value) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let reference = temporal_reference_now()?;
    parse_to_utc_at_reference(val, reference)
}

/// Parse `val` into UTC, then encode per `fmt` (predicate / expression **input** only).
pub fn normalize_temporal_value(val: Value, fmt: TemporalWireFormat) -> Result<Value, String> {
    let dt = parse_to_utc(&val)?;

    Ok(encode_utc_datetime(dt, fmt))
}

fn encode_utc_datetime(dt: chrono::DateTime<chrono::Utc>, fmt: TemporalWireFormat) -> Value {
    match fmt {
        TemporalWireFormat::Rfc3339 => Value::String(dt.to_rfc3339()),
        TemporalWireFormat::UnixMs => Value::Integer(dt.timestamp_millis()),
        TemporalWireFormat::UnixSec => Value::Integer(dt.timestamp()),
        TemporalWireFormat::Iso8601Date => {
            let d = dt.naive_utc().date();
            Value::String(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()))
        }
    }
}

/// Render an encoded temporal [`Value`] as a wire string (for URL query params).
pub fn temporal_encoded_as_wire_string(encoded: &Value) -> Result<String, String> {
    match encoded {
        Value::String(s) | Value::PhraseIdent(s) => Ok(s.clone()),
        Value::Integer(value) => Ok(value.to_string()),
        Value::Float(value) if value.is_finite() => Ok(value.to_string()),
        other => Err(format!(
            "{} is not an encoded temporal scalar",
            other.type_name()
        )),
    }
}

/// True when `s` should pass through unchanged on wire (relative `now*` tokens, opaque digit strings).
fn is_opaque_wire_temporal_string(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    t.to_ascii_lowercase().starts_with("now")
}

/// Encode a temporal for URL/query wire slots: pass through `now*` / digit tokens; else parse and format.
pub fn wire_temporal_value(val: Value, fmt: TemporalWireFormat) -> Result<Value, String> {
    if let Value::String(s) = &val {
        if is_opaque_wire_temporal_string(s) {
            return Ok(Value::String(s.trim().to_string()));
        }
    }
    let encoded = normalize_temporal_value(val, fmt)?;
    Ok(Value::String(temporal_encoded_as_wire_string(&encoded)?))
}

/// Parse a wire-format name (`unix_ms`, `rfc3339`, …) for view templates and filters.
pub fn temporal_wire_format_from_name(name: &str) -> Result<TemporalWireFormat, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "rfc3339" => Ok(TemporalWireFormat::Rfc3339),
        "unix_ms" => Ok(TemporalWireFormat::UnixMs),
        "unix_sec" => Ok(TemporalWireFormat::UnixSec),
        "iso8601_date" => Ok(TemporalWireFormat::Iso8601Date),
        other => Err(format!(
            "unknown temporal wire format `{other}` (expected rfc3339, unix_ms, unix_sec, iso8601_date)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};

    #[test]
    fn iso_string_to_unix_ms() {
        let v = normalize_temporal_value(
            Value::String("2024-06-01T12:00:00Z".to_string()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert!(matches!(v, Value::Integer(_)));
    }

    #[test]
    fn unix_ms_integer_passthrough_scale() {
        let ms = 1_717_234_567_890_i64;
        let v = normalize_temporal_value(Value::Integer(ms), TemporalWireFormat::UnixMs).unwrap();
        assert_eq!(v, Value::Integer(ms));
    }

    #[test]
    fn bare_now_string_resolves_to_current_instant() {
        let v = normalize_temporal_value(Value::String("now".into()), TemporalWireFormat::UnixMs)
            .unwrap();
        assert!(matches!(v, Value::Integer(_)));
    }

    #[test]
    fn next_week_hyphen_normalizes_to_relative_resolution() {
        let token = "next-week";
        assert!(!token.chars().all(|c| c.is_ascii_digit()));
        let r = normalize_temporal_value(Value::String(token.into()), TemporalWireFormat::UnixMs)
            .unwrap();
        assert!(
            matches!(r, Value::Integer(_)),
            "next-week → next week → midnight today + 7d as unix_ms, got {r:?}"
        );
    }

    #[test]
    fn next_week_spaced_matches_hyphen() {
        let a = normalize_temporal_value(
            Value::String("next-week".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        let b = normalize_temporal_value(
            Value::String("next week".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn next_year_spaced_matches_hyphen() {
        let a = normalize_temporal_value(
            Value::String("next-year".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        let b = normalize_temporal_value(
            Value::String("next year".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn last_year_spaced_matches_hyphen() {
        let a = normalize_temporal_value(
            Value::String("last-year".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        let b = normalize_temporal_value(
            Value::String("last year".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn wire_temporal_passes_now_offset_unchanged() {
        let v = wire_temporal_value(Value::String("now-1h".into()), TemporalWireFormat::UnixMs)
            .unwrap();
        assert_eq!(v, Value::String("now-1h".into()));
    }

    #[test]
    fn wire_temporal_passes_digit_string_unchanged() {
        let v = wire_temporal_value(
            Value::String("1717234567890".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(v, Value::String("1717234567890".into()));
    }

    #[test]
    fn wire_temporal_rfc3339_to_unix_ms_string() {
        let v = wire_temporal_value(
            Value::String("2024-06-01T12:00:00Z".to_string()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert!(matches!(v, Value::String(_)));
        let s = match v {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };
        assert!(s.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn wire_temporal_nl_phrase_encodes_unix_ms() {
        let v =
            wire_temporal_value(Value::String("today".into()), TemporalWireFormat::UnixMs).unwrap();
        assert!(matches!(v, Value::String(_)));
    }

    #[test]
    fn predicate_now_still_resolves_instant() {
        let v = normalize_temporal_value(Value::String("now".into()), TemporalWireFormat::UnixMs)
            .unwrap();
        assert!(matches!(v, Value::Integer(_)));
    }

    #[test]
    fn this_year_is_jan_first_midnight_utc() {
        let today = Utc::now().date_naive();
        let jan1 = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap();
        let expected_ms = utc_midnight(jan1).timestamp_millis();
        let v = normalize_temporal_value(
            Value::String("this year".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(v, Value::Integer(expected_ms));
    }

    #[test]
    fn rewrite_now_hyphen_offset_to_ago() {
        assert_eq!(rewrite_temporal_aliases("now-7d").as_ref(), "7d ago");
        assert_eq!(rewrite_temporal_aliases("now-1h").as_ref(), "1h ago");
        assert_eq!(
            rewrite_temporal_aliases_in_predicate_body("now-7d").as_ref(),
            "\"7d ago\""
        );
    }

    #[test]
    fn rewrite_kusto_and_now_call_forms() {
        assert_eq!(rewrite_temporal_aliases("now() - 7d").as_ref(), "7d ago");
        assert_eq!(rewrite_temporal_aliases("now()-7d").as_ref(), "7d ago");
        assert_eq!(
            rewrite_temporal_aliases(r#"datetime(now()) - duration(7, "day")"#).as_ref(),
            "7 days ago"
        );
        assert_eq!(rewrite_temporal_aliases("now()").as_ref(), "now");
        assert_eq!(
            rewrite_temporal_aliases("start_of_day(today - 6 days)").as_ref(),
            "6 days ago"
        );
    }

    #[test]
    fn normalize_accepts_rewritten_illegal_forms() {
        for s in [
            "now-7d",
            "now() - 7d",
            r#"datetime(now()) - duration(7, "day")"#,
            "7d ago",
            "7 days ago",
        ] {
            normalize_temporal_value(Value::String(s.into()), TemporalWireFormat::Rfc3339)
                .unwrap_or_else(|e| panic!("normalize {s:?}: {e}"));
        }
        // Wire lane still passes opaque now* through unchanged.
        let w = wire_temporal_value(Value::String("now-7d".into()), TemporalWireFormat::Rfc3339)
            .unwrap();
        assert_eq!(w, Value::String("now-7d".into()));
    }

    #[test]
    fn rewrite_preserves_unrelated_predicate_text() {
        let body = r#"status=requested, amount>0"#;
        assert_eq!(
            rewrite_temporal_aliases_in_predicate_body(body).as_ref(),
            body
        );
    }

    /// Documented predicate-normalize surface (fixed phrases + chrono-english UK).
    /// Keep ablation case families aligned with this list — not with a single "7d" demo.
    #[test]
    fn normalize_accepts_documented_algorithm_surface() {
        let ok = [
            "today",
            "yesterday",
            "tomorrow",
            "last week",
            "next week",
            "last month",
            "next month",
            "last year",
            "next year",
            "this year",
            "7d ago",
            "7 days ago",
            "3d ago",
            "3 hours ago",
            "2 weeks ago",
            "1h ago",
            "30 minutes ago",
            "90 minutes ago",
            "90m ago",
            "3 weeks ago",
            "48h ago",
            "10d ago",
            "5d ago",
            "1d ago",
            "0d ago",
            "1 day ago",
            "0 days ago",
            "last monday",
            "last friday",
            "next tuesday",
            "last sunday",
            "last-monday", // hyphen → space when digitless
            "now",
        ];
        for s in ok {
            normalize_temporal_value(Value::String(s.into()), TemporalWireFormat::Rfc3339)
                .unwrap_or_else(|e| panic!("expected Ok for {s:?}: {e}"));
        }
        // Known chrono-english gaps — do not score these as preferred golds.
        for s in [
            "in 2 days",
            "a week ago",
            "fortnight ago",
            "previous monday",
        ] {
            assert!(
                normalize_temporal_value(Value::String(s.into()), TemporalWireFormat::Rfc3339)
                    .is_err(),
                "unexpected Ok for unsupported {s:?}"
            );
        }
    }

    #[test]
    fn plasm_temporal_now_env_parses_task_datetime() {
        let dt = parse_temporal_now_env("2023-06-03T23:58:00").expect("parse");
        assert_eq!(dt.to_rfc3339(), "2023-06-03T23:58:00+00:00");
    }

    #[test]
    fn seven_days_ago_anchors_to_reference_instant_not_host_now() {
        let reference = parse_temporal_now_env("2023-06-03T23:58:00").expect("parse");
        let v = parse_to_utc_at_reference(&Value::String("7 days ago".into()), reference)
            .expect("7 days ago");
        let cutoff = v.to_rfc3339();
        assert!(
            cutoff.starts_with("2023-05-2"),
            "expected late May 2023 cutoff, got {cutoff}"
        );
    }

    #[test]
    fn compact_0d_1d_ago_match_today_yesterday_midnight() {
        let today =
            normalize_temporal_value(Value::String("today".into()), TemporalWireFormat::UnixMs)
                .unwrap();
        let zero =
            normalize_temporal_value(Value::String("0d ago".into()), TemporalWireFormat::UnixMs)
                .unwrap();
        assert_eq!(today, zero, "0d ago must be calendar today midnight");

        let yesterday = normalize_temporal_value(
            Value::String("yesterday".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        let one =
            normalize_temporal_value(Value::String("1d ago".into()), TemporalWireFormat::UnixMs)
                .unwrap();
        let one_long = normalize_temporal_value(
            Value::String("1 day ago".into()),
            TemporalWireFormat::UnixMs,
        )
        .unwrap();
        assert_eq!(yesterday, one, "1d ago must be calendar yesterday midnight");
        assert_eq!(one, one_long);
    }
}
