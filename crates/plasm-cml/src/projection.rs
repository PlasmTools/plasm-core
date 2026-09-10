//! Pure, declared URL extraction. Errors intentionally exclude input URL values.

use crate::CmlError;
use indexmap::IndexMap;
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UrlPathPart {
    Literal { value: String },
    Capture { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlProjection {
    pub origins: Vec<String>,
    pub path: Vec<UrlPathPart>,
    /// Output field name to query parameter name.
    #[serde(default)]
    pub query: IndexMap<String, String>,
}

fn invalid(message: &str) -> CmlError {
    CmlError::InvalidTemplate {
        message: message.into(),
    }
}

fn decode_segment(segment: &str) -> Result<String, CmlError> {
    let mut result = Vec::with_capacity(segment.len());
    let mut bytes = segment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|b| (b as char).to_digit(16));
            let low = bytes.next().and_then(|b| (b as char).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => result.push((high * 16 + low) as u8),
                _ => return Err(invalid("URL contains malformed percent encoding")),
            }
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).map_err(|_| invalid("URL contains invalid UTF-8"))
}

impl UrlProjection {
    pub fn validate(&self) -> Result<(), CmlError> {
        if self.origins.is_empty() {
            return Err(invalid("URL projection requires approved origins"));
        }
        for origin in &self.origins {
            let url = Url::parse(origin).map_err(|_| invalid("invalid declared URL origin"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
            {
                return Err(invalid(
                    "URL origin declaration must contain only an HTTP(S) origin",
                ));
            }
        }
        let mut fields = BTreeSet::new();
        for part in &self.path {
            match part {
                UrlPathPart::Literal { value }
                    if value.is_empty()
                        || value.contains(['/', '\\'])
                        || matches!(value.as_str(), "." | "..") =>
                {
                    return Err(invalid(
                        "URL path literals must be nonempty individual segments",
                    ));
                }
                UrlPathPart::Capture { name } if name.is_empty() || !fields.insert(name) => {
                    return Err(invalid(
                        "URL projection output names must be nonempty and unique",
                    ));
                }
                _ => {}
            }
        }
        for (name, parameter) in &self.query {
            if name.is_empty() || parameter.is_empty() || !fields.insert(name) {
                return Err(invalid(
                    "URL projection output names must be nonempty and unique",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn evaluate(&self, value: Value) -> Result<Value, CmlError> {
        self.validate()?;
        let raw = match value {
            Value::Null => return Ok(Value::Null),
            Value::String(raw) => raw,
            _ => return Err(invalid("URL projection requires a string or null")),
        };
        // Check the input before Url's normalizations can hide malformed escapes or backslashes.
        decode_segment(&raw)?;
        if raw.contains('\\') || raw.chars().any(char::is_control) {
            return Err(invalid("URL contains invalid characters"));
        }
        if !raw.starts_with("https://") && !raw.starts_with("http://") {
            return Err(invalid("URL must have an explicit HTTP(S) authority"));
        }
        let url = Url::parse(&raw).map_err(|_| invalid("invalid URL"))?;
        // Url normalizes dot segments; reject them before normalization so the declared
        // path contract cannot be satisfied through a different raw resource path.
        if let Some((_, authority_and_path)) = raw.split_once("://") {
            if let Some((_, path)) = authority_and_path.split_once('/') {
                let path = path.split(['?', '#']).next().unwrap_or_default();
                for segment in path.split('/') {
                    if matches!(decode_segment(segment)?.as_str(), "." | "..") {
                        return Err(invalid("URL dot segments are not permitted"));
                    }
                }
            }
        }
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(invalid("URL userinfo and fragments are not permitted"));
        }
        let origin = url.origin();
        if !self
            .origins
            .iter()
            .any(|allowed| Url::parse(allowed).is_ok_and(|a| a.origin() == origin))
        {
            return Err(invalid("URL origin is not permitted"));
        }
        let segments: Vec<_> = url
            .path_segments()
            .ok_or_else(|| invalid("URL has no path"))?
            .collect();
        if segments.len() != self.path.len() {
            return Err(invalid("URL path does not match declared shape"));
        }
        let mut output = IndexMap::new();
        for (segment, part) in segments.into_iter().zip(&self.path) {
            let segment = decode_segment(segment)?;
            if segment.is_empty()
                || segment.contains(['/', '\\'])
                || segment.chars().any(char::is_control)
            {
                return Err(invalid("URL identity must be a nonempty path segment"));
            }
            match part {
                UrlPathPart::Literal { value } if *value != segment => {
                    return Err(invalid("URL path does not match declared shape"))
                }
                UrlPathPart::Capture { name } => {
                    output.insert(name.clone(), Value::String(segment));
                }
                _ => {}
            }
        }
        for (field, parameter) in &self.query {
            let mut values = url.query_pairs().filter(|(key, _)| key == parameter);
            if let Some((_, value)) = values.next() {
                if values.next().is_some() {
                    return Err(invalid("URL contains duplicate selected query parameters"));
                }
                output.insert(field.clone(), Value::String(value.into_owned()));
            }
        }
        Ok(Value::Object(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{eval_cml, CmlEnv, CmlExpr};
    use serde_json::json;

    fn eval(expression: serde_json::Value, inputs: serde_json::Value) -> Result<Value, CmlError> {
        let expression: CmlExpr = serde_json::from_value(
            crate::wire_normalize::normalize_wire_cml_template(expression),
        )
        .unwrap();
        let env: CmlEnv = serde_json::from_value(inputs).unwrap();
        eval_cml(&expression, &env)
    }

    #[test]
    fn optional_values_preserve_false_zero_and_empty() {
        for value in [json!(false), json!(0), json!("")] {
            let actual = eval(json!({"type":"first_present","values":[{"type":"var","name":"absent","if":{"exists":"absent"}},{"type":"var","name":"value"}]}), json!({"value": value})).unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), value);
        }
        assert!(eval(
            json!({"type":"trim","value":{"type":"const","value":false}}),
            json!({})
        )
        .is_err());
        assert!(eval(
            json!({"type":"first_present","values":[{"type":"var","name":"missing"}]}),
            json!({})
        )
        .is_err());
    }

    #[test]
    fn locals_are_immutable_and_input_collection_respects_scope() {
        let expression = json!({"type":"let","bindings":[{"name":"clean","value":{"type":"trim","value":{"type":"var","name":"raw"}}}],"value":{"type":"first_present","values":[{"type":"var","name":"clean"},{"type":"var","name":"default"}]}});
        assert_eq!(
            eval(expression.clone(), json!({"raw":" ","default":"selected"})).unwrap(),
            Value::String("selected".into())
        );
        assert!(eval(expression.clone(), json!({"raw":"x","clean":"shadow"})).is_err());
        let parsed = serde_json::from_value(expression).unwrap();
        let mut vars = indexmap::IndexSet::new();
        crate::transport::collect_expr_vars(&parsed, &mut vars);
        assert_eq!(vars.into_iter().collect::<Vec<_>>(), ["raw", "default"]);
    }

    #[test]
    fn field_projection_is_typed_and_null_propagating() {
        let expression =
            json!({"type":"field","value":{"type":"var","name":"row"},"path":["nested","value"]});
        assert_eq!(
            eval(
                expression.clone(),
                json!({"row":{"nested":{"value":false}}})
            )
            .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            eval(expression.clone(), json!({"row":{}})).unwrap(),
            Value::Null
        );
        assert!(eval(expression, json!({"row":{"nested":5}})).is_err());
    }

    #[test]
    fn url_projection_enforces_declared_origin_shape_and_uniqueness() {
        let expression = json!({"type":"url_project","value":{"type":"var","name":"link"},"projection":{"origins":["https://example.test"],"path":[{"kind":"literal","value":"records"},{"kind":"capture","name":"key"}],"query":{"credential":"grant"}}});
        assert_eq!(
            serde_json::to_value(
                eval(
                    expression.clone(),
                    json!({"link":"https://example.test:443/records/a%20b?grant=x%2By"})
                )
                .unwrap()
            )
            .unwrap(),
            json!({"key":"a b","credential":"x+y"})
        );
        for link in [
            "https:/example.test/records/x/../a",
            "https://example.test/records/x/../a",
            "https://other.test/records/a?grant=secret",
            "https://example.test/prefix/records/a",
            "https://example.test/records/a?grant=x&grant=y",
            "https://user@example.test/records/a",
            "https://example.test/records/a#x",
            "https://example.test/records/a%2Fb",
            "https://example.test/records/a%zz",
            "https://example.test/records/a?grant=%FF",
        ] {
            let error = eval(expression.clone(), json!({"link":link}))
                .unwrap_err()
                .to_string();
            assert!(!error.contains(link));
            assert!(!error.contains("secret"));
        }
        assert_eq!(eval(expression, json!({"link":null})).unwrap(), Value::Null);
    }
}
