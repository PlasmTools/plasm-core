//! Temporal parsing and wire encoding for Plasm.
//!
//! **Predicate / expression input** — [`normalize_temporal_value`]: values typed as
//! [`FieldType::Date`] in path expressions and predicates (see [`crate::expr_parser`]). Bare `now`
//! resolves to the current UTC instant.
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
//! (case-insensitive) resolves to the current UTC instant here — [`parse_date_string`] does not
//! treat `now` as a special token.
//!
//! **Pre-normalisation:** strings with **no ASCII digits** have `-` replaced with spaces so
//! `next-week` becomes `next week` before [`parse_date_string`] and before fixed relative-phrase
//! resolution. (Hyphenated ISO dates and timestamps keep their `-` because they contain digits.)
//!
//! **Fixed relative English phrases** (`today`, `next week`, `this year`, …) are resolved here
//! with chrono for stable semantics; everything else goes to `chrono-english` with
//! [`Dialect::Uk`](chrono_english::Dialect).

use chrono::{Datelike, Duration, Months, NaiveDate, TimeZone, Utc};
use chrono_english::{parse_date_string, Dialect};

use crate::{TemporalWireFormat, Value};

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

/// Common relative phrases for predicate input. Times are **midnight UTC** on the resolved
/// calendar day, except `now` (handled above) which uses the live instant.
fn resolve_relative_english_phrase(lowercase_ascii: &str) -> Option<chrono::DateTime<Utc>> {
    let today = Utc::now().date_naive();
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
        _ => None,
    }
}

fn parse_to_utc(val: &Value) -> Result<chrono::DateTime<chrono::Utc>, String> {
    match val {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Err("empty date string".to_string());
            }
            if t.eq_ignore_ascii_case("now") {
                return Ok(Utc::now());
            }
            if t.chars().all(|c| c.is_ascii_digit()) {
                let n: i64 = t
                    .parse()
                    .map_err(|_| format!("invalid integer timestamp: {t}"))?;
                return datetime_from_integer(n);
            }
            let normalized = normalize_natural_language_temporal_input(t);
            let lower = normalized.to_ascii_lowercase();
            if let Some(dt) = resolve_relative_english_phrase(&lower) {
                return Ok(dt);
            }
            parse_date_string(&normalized, Utc::now(), Dialect::Uk).map_err(|e| e.to_string())
        }
        Value::Integer(i) => datetime_from_integer(*i),
        Value::Float(f) => datetime_from_float(*f),
        _ => Err(format!("cannot interpret {} as date/time", val.type_name())),
    }
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
pub fn temporal_encoded_as_wire_string(encoded: &Value) -> String {
    match encoded {
        Value::String(s) | Value::PhraseIdent(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                (*f as i64).to_string()
            } else {
                f.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(&plasm_value_to_json_temporal(encoded)).unwrap_or_default()
        }
        Value::PlasmInputRef(_) | Value::UnionCtor { .. } | Value::Money(_) => String::new(),
    }
}

fn plasm_value_to_json_temporal(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::json!(b),
        Value::Integer(i) => serde_json::json!(i),
        Value::Float(f) => serde_json::json!(f),
        Value::String(s) | Value::PhraseIdent(s) => serde_json::json!(s),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(plasm_value_to_json_temporal).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), plasm_value_to_json_temporal(v));
            }
            serde_json::Value::Object(map)
        }
        Value::PlasmInputRef(_) | Value::UnionCtor { .. } | Value::Money(_) => {
            serde_json::Value::Null
        }
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
    Ok(Value::String(temporal_encoded_as_wire_string(&encoded)))
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
}
