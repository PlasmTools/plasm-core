use crate::value::Value;

pub(crate) fn render_surface_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) | Value::PhraseIdent(s) => render_bare_or_quoted_string(s),
        Value::PlasmInputRef(_) => "$".to_string(),
        Value::UnionCtor {
            ctor_label,
            ctor_fields,
        } => {
            if ctor_fields.is_empty() {
                ctor_label.clone()
            } else {
                let parts: Vec<String> = ctor_fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", render_surface_value(v)))
                    .collect();
                format!("{ctor_label}{{{}}}", parts.join(", "))
            }
        }
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(render_surface_value)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}={}", render_surface_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Money(m) => m.display(),
    }
}

pub(crate) fn render_bare_or_quoted_string(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return s.to_string();
    }
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(crate) fn render_id_slot(s: &str) -> String {
    if s == "$" {
        "$".to_string()
    } else if s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        s.to_string()
    } else {
        render_surface_value(&Value::String(s.to_string()))
    }
}
