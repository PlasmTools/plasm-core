//! Minijinja rendering for CGS view `kind: computed` output bindings.

use indexmap::IndexMap;
use minijinja::{value::ValueKind, Environment, UndefinedBehavior};
use plasm_core::{temporal_wire_format_from_name, wire_temporal_value, Value};
use serde_json::json;

use crate::RuntimeError;

const VIEW_TEMPLATE_MAX_CHARS: usize = 32_768;

fn plasm_value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => json!(b),
        Value::Integer(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::String(s) | Value::PhraseIdent(s) => json!(s),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(plasm_value_to_json).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), plasm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        Value::PlasmInputRef(_) | Value::UnionCtor { .. } => serde_json::Value::Null,
    }
}

fn register_view_template_filters(env: &mut Environment<'_>) {
    env.add_filter(
        "urlencode",
        |s: String| -> Result<String, minijinja::Error> {
            Ok(url::form_urlencoded::byte_serialize(s.as_bytes()).collect())
        },
    );
    env.add_filter(
        "wire_query_suffix",
        |json_text: String| -> Result<String, minijinja::Error> {
            let t = json_text.trim();
            if t.is_empty() || t == "null" {
                return Ok(String::new());
            }
            let v: serde_json::Value = serde_json::from_str(t).map_err(|e| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("wire_query_suffix: invalid JSON object: {e}"),
                )
            })?;
            let Some(obj) = v.as_object() else {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "wire_query_suffix: expected JSON object",
                ));
            };
            if obj.is_empty() {
                return Ok(String::new());
            }
            let mut parts = Vec::new();
            for (k, val) in obj {
                let val_s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                parts.push(format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(val_s.as_bytes()).collect::<String>()
                ));
            }
            if parts.is_empty() {
                Ok(String::new())
            } else {
                Ok(format!("&{}", parts.join("&")))
            }
        },
    );
    env.add_filter("strip_trailing_slash", |s: String| -> String {
        s.trim_end_matches('/').to_string()
    });
    env.add_filter(
        "json_encode",
        |v: minijinja::Value| -> Result<String, minijinja::Error> {
            let plasm = minijinja_to_plasm(v);
            let json = plasm_value_to_json(&plasm);
            serde_json::to_string(&json).map_err(|e| {
                minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string())
            })
        },
    );
    env.add_filter(
        "split",
        |s: String, sep: String| -> Result<Vec<String>, minijinja::Error> {
            if sep.is_empty() {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "split: separator must be non-empty",
                ));
            }
            Ok(s.split(&sep).map(str::to_string).collect())
        },
    );
    env.add_filter(
        "split_part",
        |s: String, sep: String, index: i64| -> Result<String, minijinja::Error> {
            if sep.is_empty() {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "split_part: separator must be non-empty",
                ));
            }
            let idx = usize::try_from(index.max(0)).unwrap_or(0);
            Ok(s.split(&sep).nth(idx).unwrap_or("").to_string())
        },
    );
    env.add_filter(
        "wire_time",
        |v: minijinja::Value, format: String| -> Result<String, minijinja::Error> {
            let fmt = temporal_wire_format_from_name(&format)
                .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e))?;
            let plasm = minijinja_to_plasm(v);
            let out = wire_temporal_value(plasm, fmt)
                .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e))?;
            Ok(match out {
                Value::String(s) | Value::PhraseIdent(s) => s,
                Value::Integer(i) => i.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                Value::Array(_) | Value::Object(_) => {
                    serde_json::to_string(&plasm_value_to_json(&out)).unwrap_or_default()
                }
                Value::PlasmInputRef(_) | Value::UnionCtor { .. } => String::new(),
            })
        },
    );
}

fn minijinja_to_plasm(v: minijinja::Value) -> Value {
    match v.kind() {
        ValueKind::None | ValueKind::Undefined => Value::Null,
        ValueKind::Bool => Value::Bool(v.is_true()),
        ValueKind::String => Value::String(v.as_str().unwrap_or_default().to_string()),
        ValueKind::Number => {
            if let Some(i) = v.as_i64() {
                Value::Integer(i)
            } else {
                let s = v.to_string();
                Value::Float(s.parse().unwrap_or(0.0))
            }
        }
        ValueKind::Seq => {
            let mut items = Vec::new();
            if let Ok(iter) = v.try_iter() {
                for elem in iter {
                    items.push(minijinja_to_plasm(elem));
                }
            }
            Value::Array(items)
        }
        ValueKind::Map | ValueKind::Iterable | ValueKind::Plain => {
            if let Some(s) = v.as_str() {
                Value::String(s.to_string())
            } else {
                Value::String(v.to_string())
            }
        }
        ValueKind::Bytes => {
            Value::String(String::from_utf8_lossy(v.as_bytes().unwrap_or_default()).into())
        }
        ValueKind::Invalid | _ => Value::Null,
    }
}

/// Evaluate a view computed-field template against scope and materialized output fields.
pub fn render_view_computed_template(
    template: &str,
    scope: &IndexMap<String, Value>,
    fields_plain: &IndexMap<String, Value>,
) -> Result<Value, RuntimeError> {
    render_view_template_with_nodes(template, scope, fields_plain, &IndexMap::new())
}

/// Evaluate a view node-parameter bind template against scope and prior node first-row fields.
pub fn render_view_param_bind_template(
    template: &str,
    scope: &IndexMap<String, Value>,
    node_fields: &IndexMap<String, IndexMap<String, Value>>,
) -> Result<Value, RuntimeError> {
    render_view_template_with_nodes(template, scope, &IndexMap::new(), node_fields)
}

/// Rewrite agent-friendly `.split('sep')[n]` into `| split_part('sep', n)` for view templates.
pub fn desugar_view_computed_template(template: &str) -> String {
    let mut s = template.to_string();
    while let Some(dot) = s.find(".split(") {
        let expr_start = s[..dot]
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_')
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        let expr = s[expr_start..dot].trim();
        if expr.is_empty() || !expr.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        let Some((sep, index, span_len)) = parse_split_index_suffix(&s[dot..]) else {
            break;
        };
        let replacement = format!(" | split_part('{sep}', {index})");
        let end = dot + span_len;
        s.replace_range(dot..end, &replacement);
    }
    s
}

fn parse_split_index_suffix(s: &str) -> Option<(String, usize, usize)> {
    let prefix = ".split(";
    if !s.starts_with(prefix) {
        return None;
    }
    let after_paren = &s[prefix.len()..];
    let (sep, after_sep) = parse_quoted_sep(after_paren)?;
    let mut tail = after_sep.trim_start();
    if let Some(rest) = tail.strip_prefix(')') {
        tail = rest.trim_start();
    }
    if !tail.starts_with('[') {
        return None;
    }
    let tail = &tail[1..];
    let end = tail.find(']')?;
    let index: usize = tail[..end].trim().parse().ok()?;
    let span_len = s.len() - tail[end + 1..].len();
    Some((sep, index, span_len))
}

fn parse_quoted_sep(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let q = s.chars().next()?;
    if q != '\'' && q != '"' {
        return None;
    }
    let mut sep = String::new();
    let mut escaped = false;
    for (i, ch) in s.char_indices().skip(1) {
        if escaped {
            sep.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == q {
            return Some((sep, &s[i + ch.len_utf8()..]));
        }
        sep.push(ch);
    }
    None
}

fn render_view_template_with_nodes(
    template: &str,
    scope: &IndexMap<String, Value>,
    fields_plain: &IndexMap<String, Value>,
    node_fields: &IndexMap<String, IndexMap<String, Value>>,
) -> Result<Value, RuntimeError> {
    let trimmed = desugar_view_computed_template(template.trim());
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return Err(RuntimeError::ConfigurationError {
            message: "computed view template must be non-empty".into(),
        });
    }
    if trimmed.chars().count() > VIEW_TEMPLATE_MAX_CHARS {
        return Err(RuntimeError::ConfigurationError {
            message: format!("computed view template exceeds {VIEW_TEMPLATE_MAX_CHARS} characters"),
        });
    }

    let mut ctx = serde_json::Map::new();
    for (k, v) in scope {
        ctx.insert(k.clone(), plasm_value_to_json(v));
    }
    for (k, v) in fields_plain {
        ctx.insert(k.clone(), plasm_value_to_json(v));
    }
    let mut nodes_obj = serde_json::Map::new();
    for (node_id, fields) in node_fields {
        let mut field_obj = serde_json::Map::new();
        for (fk, fv) in fields {
            field_obj.insert(fk.clone(), plasm_value_to_json(fv));
        }
        let node_json = serde_json::Value::Object(field_obj.clone());
        nodes_obj.insert(node_id.clone(), node_json.clone());
        ctx.insert(node_id.clone(), node_json);
    }
    if !nodes_obj.is_empty() {
        ctx.insert("nodes".to_string(), serde_json::Value::Object(nodes_obj));
    }

    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    register_view_template_filters(&mut env);

    let tmpl = env
        .template_from_str(trimmed)
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!("computed view template compile error: {e}"),
        })?;

    let rendered = tmpl
        .render(ctx)
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!("computed view template render error: {e}"),
        })?;

    Ok(Value::String(rendered))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_time_filter_in_template() {
        let mut scope = IndexMap::new();
        scope.insert("from".to_string(), Value::String("now-1h".into()));
        let out = render_view_computed_template(
            "{{ from | wire_time('unix_ms') }}",
            &scope,
            &IndexMap::new(),
        )
        .unwrap();
        assert_eq!(out, Value::String("now-1h".into()));
    }

    #[test]
    fn urlencode_filter_in_template() {
        let out = render_view_computed_template(
            "{{ 'a=b&c' | urlencode }}",
            &IndexMap::new(),
            &IndexMap::new(),
        )
        .unwrap();
        assert_eq!(out, Value::String("a%3Db%26c".into()));
    }

    #[test]
    fn wire_query_suffix_appends_params() {
        let out = render_view_computed_template(
            "{{ '{\"foo\":\"bar\"}' | wire_query_suffix }}",
            &IndexMap::new(),
            &IndexMap::new(),
        )
        .unwrap();
        assert_eq!(out, Value::String("&foo=bar".into()));
    }

    #[test]
    fn split_part_filter_extracts_team_key() {
        let mut scope = IndexMap::new();
        scope.insert(
            "issue_identifier".to_string(),
            Value::String("EVA-60".into()),
        );
        let out = render_view_computed_template(
            "{{ issue_identifier | split_part('-', 0) }}",
            &scope,
            &IndexMap::new(),
        )
        .unwrap();
        assert_eq!(out, Value::String("EVA".into()));
    }

    #[test]
    fn desugared_split_in_set_statement() {
        let mut scope = IndexMap::new();
        scope.insert(
            "issue_identifier".to_string(),
            Value::String("EVA-60".into()),
        );
        let raw = "{%- set team_key = issue_identifier.split('-')[0] -%}\n{{ team_key }}";
        let out = render_view_computed_template(raw, &scope, &IndexMap::new()).unwrap();
        assert_eq!(out, Value::String("EVA".into()));
    }

    #[test]
    fn desugar_split_index_to_filter_pipe() {
        let raw = "issue_identifier.split('-')[0]";
        assert_eq!(
            desugar_view_computed_template(raw),
            "issue_identifier | split_part('-', 0)"
        );
    }

    #[test]
    fn template_sees_prior_output_fields() {
        let mut fields = IndexMap::new();
        fields.insert("app_url".to_string(), Value::String("http://x/".into()));
        let out = render_view_computed_template(
            "{{ app_url | strip_trailing_slash }}/d/foo",
            &IndexMap::new(),
            &fields,
        )
        .unwrap();
        assert_eq!(out, Value::String("http://x/d/foo".into()));
    }
}
