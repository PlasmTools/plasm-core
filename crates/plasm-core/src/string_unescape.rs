//! JSON-style unescaping for capability inputs whose schema marks structured string semantics.
//!
//! Quoted Plasm string literals already apply JSON-style escapes at parse time (`\n`, `\t`, …) in
//! [`crate::expr_parser`]. This module remains for **post-parse** normalization of structured /
//! multiline string fields when a host may have double-encoded backslash sequences (literal `\` +
//! `n` still present in a [`crate::Value::String`]). See
//! [`crate::schema::StringSemantics::is_structured_or_multiline`].

use crate::schema::{InputType, CGS};
use crate::value::{parse_json_subtree_str, FieldType, Value};

/// Single-character JSON-style escapes after `\` (not `\uXXXX`).
/// Shared by the quoted-string lexer (strict) and post-parse normalization (lenient).
#[must_use]
pub fn json_escape_simple(c: char) -> Option<char> {
    match c {
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        '\\' => Some('\\'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '/' => Some('/'),
        'b' => Some('\u{8}'),
        'f' => Some('\u{c}'),
        _ => None,
    }
}

/// Decode `\uXXXX` from four hex digits already collected.
#[must_use]
pub fn unicode_escape_from_hex(hex: &str) -> Option<char> {
    if hex.len() != 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
}

/// Unescape common JSON-style backslash sequences (`\n`, `\t`, `\uXXXX`, etc.).
/// Unknown escapes are preserved as `\` + char (lenient; the lexer rejects them).
pub fn unescape_json_style_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(esc) => match json_escape_simple(esc) {
                Some(ch) => out.push(ch),
                None if esc == 'u' => {
                    let mut h = String::with_capacity(4);
                    let mut early = None;
                    for _ in 0..4 {
                        match it.next() {
                            Some(ch) if ch.is_ascii_hexdigit() => h.push(ch),
                            Some(ch) => {
                                early = Some(ch);
                                break;
                            }
                            None => break,
                        }
                    }
                    if let Some(ch) = unicode_escape_from_hex(&h) {
                        out.push(ch);
                    } else {
                        out.push('\\');
                        out.push('u');
                        out.push_str(&h);
                        if let Some(ch) = early {
                            out.push(ch);
                        }
                    }
                }
                None => {
                    out.push('\\');
                    out.push(esc);
                }
            },
            None => out.push('\\'),
        }
    }
    out
}

/// Apply [`unescape_json_style_escapes`] to string fields marked as structured in `input_type`.
pub fn normalize_structured_string_inputs(
    value: Value,
    input_type: &InputType,
    cgs: &CGS,
) -> Value {
    match input_type {
        InputType::None | InputType::Union { .. } => value,
        InputType::Value { field_type, .. } => {
            if matches!(field_type, FieldType::Json) {
                if let Value::String(s) = value {
                    let candidate = unescape_json_style_escapes(&s);
                    if let Some(parsed) =
                        parse_json_subtree_str(&candidate).or_else(|| parse_json_subtree_str(&s))
                    {
                        return parsed;
                    }
                    return Value::String(s);
                }
            }
            value
        }
        InputType::Object { fields, .. } => {
            let Value::Object(mut map) = value else {
                return value;
            };
            for field in fields {
                let Ok(nv) = field.named_value(cgs) else {
                    continue;
                };
                if nv.field_type == FieldType::Json {
                    if let Some(Value::String(s)) = map.get(&field.name).cloned() {
                        let candidate = unescape_json_style_escapes(&s);
                        if let Some(parsed) = parse_json_subtree_str(&candidate)
                            .or_else(|| parse_json_subtree_str(&s))
                        {
                            map.insert(field.name.clone(), parsed);
                        }
                    }
                    continue;
                }
                if nv.field_type != FieldType::String {
                    continue;
                }
                if !field
                    .effective_string_semantics(cgs)
                    .is_structured_or_multiline()
                {
                    continue;
                }
                if let Some(Value::String(s)) = map.get_mut(&field.name) {
                    let new_s = unescape_json_style_escapes(s);
                    if new_s != *s {
                        *s = new_s;
                    }
                }
            }
            Value::Object(map)
        }
        InputType::Array { element_type, .. } => {
            let Value::Array(items) = value else {
                return value;
            };
            Value::Array(
                items
                    .into_iter()
                    .map(|v| normalize_structured_string_inputs(v, element_type.as_ref(), cgs))
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        InputFieldSchema, InputFieldWire, NamedValueSchema, StringSemantics, ValueDomainKey, CGS,
    };

    #[test]
    fn unescape_basic_escapes() {
        assert_eq!(unescape_json_style_escapes("a\\nb"), "a\nb");
        assert_eq!(unescape_json_style_escapes("\\\\"), "\\");
        assert_eq!(unescape_json_style_escapes("\\\""), "\"");
    }

    #[test]
    fn unescape_unicode() {
        assert_eq!(unescape_json_style_escapes("\\u0041"), "A");
    }

    #[test]
    fn normalize_object_markdown_field() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "unescape_p2_md".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Markdown),
                array_items: None,
            },
        );
        let input_type = InputType::Object {
            fields: vec![InputFieldSchema {
                name: "p2".to_string(),
                wire: InputFieldWire::Registry(ValueDomainKey::new("unescape_p2_md").expect("key")),
                required: false,
                description: None,
                default: None,
                role: None,
                wire_json_path: None,
                wire_array_element_key: None,
                sink_class: None,
            }],
            additional_fields: false,
        };
        let v = Value::Object(
            vec![
                (
                    "p2".to_string(),
                    Value::String("line1\\n\\nline2".to_string()),
                ),
                ("id".to_string(), Value::String("x".to_string())),
            ]
            .into_iter()
            .collect(),
        );
        let out = normalize_structured_string_inputs(v, &input_type, &cgs);
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("p2").unwrap().as_str().unwrap(), "line1\n\nline2");
        assert_eq!(obj.get("id").unwrap().as_str().unwrap(), "x");
    }

    #[test]
    fn short_semantics_unchanged() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "unescape_p2_short".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        let input_type = InputType::Object {
            fields: vec![InputFieldSchema {
                name: "p2".to_string(),
                wire: InputFieldWire::Registry(
                    ValueDomainKey::new("unescape_p2_short").expect("key"),
                ),
                required: false,
                description: None,
                default: None,
                role: None,
                wire_json_path: None,
                wire_array_element_key: None,
                sink_class: None,
            }],
            additional_fields: false,
        };
        let v = Value::Object(
            vec![("p2".to_string(), Value::String("a\\nb".to_string()))]
                .into_iter()
                .collect(),
        );
        let out = normalize_structured_string_inputs(v, &input_type, &cgs);
        assert_eq!(
            out.as_object()
                .unwrap()
                .get("p2")
                .unwrap()
                .as_str()
                .unwrap(),
            "a\\nb"
        );
    }

    #[test]
    fn normalize_json_object_field_from_string() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "json_p4".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Json,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        let input_type = InputType::Object {
            fields: vec![InputFieldSchema {
                name: "p4".to_string(),
                wire: InputFieldWire::Registry(ValueDomainKey::new("json_p4").expect("key")),
                required: false,
                description: None,
                default: None,
                role: None,
                wire_json_path: None,
                wire_array_element_key: None,
                sink_class: None,
            }],
            additional_fields: false,
        };
        let v = Value::Object(
            vec![(
                "p4".to_string(),
                Value::String(r#"{"name":"t","kind":"zone"}"#.to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let out = normalize_structured_string_inputs(v, &input_type, &cgs);
        let obj = out.as_object().unwrap();
        let inner = obj.get("p4").unwrap().as_object().unwrap();
        assert_eq!(inner.get("name").unwrap().as_str().unwrap(), "t");
    }
}
