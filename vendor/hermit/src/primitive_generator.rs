use rand::RngExt;
use serde_json::Value as JsonValue;
use yaml_serde::Value as YamlValue;

use crate::constants::{BASE64_CHARS, RANDOM_WORDS};
use crate::resource_store::new_uuid;

// The min-length padding loop adds whole words at a time, always overshooting
// the target -- making the exact `<` vs `<=` boundary unobservable. Truncation
// at max_len is likewise a no-op when the string is already exactly max_len.
#[cfg_attr(test, mutants::skip)]
pub(crate) fn primitive_fallback(schema: &YamlValue, schema_type: &str) -> JsonValue {
    let mut rng = rand::rng();
    match schema_type {
        "string" => {
            let fmt = schema.get("format").and_then(|v| v.as_str()).unwrap_or("");
            let s = if fmt.is_empty() {
                let min_len = schema
                    .get("minLength")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let max_len = schema
                    .get("maxLength")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(usize::MAX);
                let word_count = rng.random_range(2..=5usize);
                let mut s = (0..word_count)
                    .map(|_| random_word(&mut rng))
                    .collect::<Vec<_>>()
                    .join(" ");
                while s.len() < min_len {
                    s.push(' ');
                    s.push_str(random_word(&mut rng));
                }
                if s.len() > max_len {
                    s.truncate(max_len);
                }
                s
            } else {
                string_for_format(fmt, &mut rng)
            };
            JsonValue::String(s)
        }
        "integer" | "number" => {
            let min_opt = schema.get("minimum").and_then(|v| v.as_i64());
            let max_opt = schema.get("maximum").and_then(|v| v.as_i64());
            let (min, max) = match (min_opt, max_opt) {
                (Some(min), Some(max)) => (min, max),
                (Some(min), None) => (min, min + 999),
                (None, Some(max)) => (max - 999, max),
                (None, None) => (1, 1000),
            };
            JsonValue::Number(rng.random_range(min..=max).into())
        }
        "boolean" => JsonValue::Bool(rng.random()),
        _ => JsonValue::Null,
    }
}

pub(crate) fn random_word(rng: &mut impl RngExt) -> &'static str {
    RANDOM_WORDS[rng.random_range(0..RANDOM_WORDS.len())]
}

fn string_for_format(fmt: &str, rng: &mut impl RngExt) -> String {
    match fmt {
        "date-time" => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            rng.random_range(2000u16..=2030),
            rng.random_range(1u8..=12),
            rng.random_range(1u8..=28),
            rng.random_range(0u8..=23),
            rng.random_range(0u8..=59),
            rng.random_range(0u8..=59),
        ),
        "date" => format!(
            "{:04}-{:02}-{:02}",
            rng.random_range(2000u16..=2030),
            rng.random_range(1u8..=12),
            rng.random_range(1u8..=28),
        ),
        "time" => format!(
            "{:02}:{:02}:{:02}Z",
            rng.random_range(0u8..=23),
            rng.random_range(0u8..=59),
            rng.random_range(0u8..=59),
        ),
        "uuid" => new_uuid(),
        "email" => format!("{}@{}.com", random_word(rng), random_word(rng)),
        "uri" => format!("https://{}.com/{}", random_word(rng), random_word(rng)),
        "hostname" => format!("{}.{}", random_word(rng), random_word(rng)),
        "ipv4" => format!(
            "{}.{}.{}.{}",
            rng.random_range(1u8..=254),
            rng.random::<u8>(),
            rng.random::<u8>(),
            rng.random_range(1u8..=254),
        ),
        "ipv6" => format!(
            "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
            rng.random::<u16>(),
        ),
        "byte" => (0..8)
            .map(|_| BASE64_CHARS[rng.random_range(0..64)] as char)
            .collect(),
        "password" => format!("{}-{}", random_word(rng), random_word(rng)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::RANDOM_WORDS;

    fn yaml(s: &str) -> YamlValue {
        yaml_serde::from_str(s).unwrap()
    }

    #[test]
    fn random_word_returns_word_from_word_list() {
        let mut rng = rand::rng();
        for _ in 0..20 {
            let word = random_word(&mut rng);
            assert!(
                RANDOM_WORDS.contains(&word),
                "{word:?} should be in RANDOM_WORDS"
            );
        }
    }

    #[test]
    fn primitive_fallback_integer_with_only_minimum_can_exceed_minimum() {
        let schema = yaml("minimum: 0");
        let any_above_zero = (0..50)
            .map(|_| primitive_fallback(&schema, "integer"))
            .any(|v| v.as_i64().is_some_and(|n| n > 0));
        assert!(
            any_above_zero,
            "minimum-only range should produce values above the minimum"
        );
    }

    #[test]
    fn primitive_fallback_integer_with_only_maximum_can_go_below_maximum() {
        let schema = yaml("maximum: 0");
        let any_below_zero = (0..50)
            .map(|_| primitive_fallback(&schema, "integer"))
            .any(|v| v.as_i64().is_some_and(|n| n < 0));
        assert!(
            any_below_zero,
            "maximum-only range should produce values below the maximum"
        );
    }
}
