//! Pure, declared URL extraction. Errors intentionally exclude input URL values.

use crate::CmlError;
use indexmap::IndexMap;
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

#[derive(Debug, Clone, thiserror::Error)]
pub enum UrlProjectionError {
    #[error("URL projection requires at least one approved origin")]
    NoOrigins,
    #[error("declared URL origin is invalid")]
    DeclaredOriginParse(#[source] std::sync::Arc<url::ParseError>),
    #[error("declared URL origin must use HTTP or HTTPS")]
    DeclaredOriginScheme,
    #[error("declared URL origin cannot contain user information")]
    DeclaredOriginUserInfo,
    #[error("declared URL origin cannot contain a query")]
    DeclaredOriginQuery,
    #[error("declared URL origin cannot contain a fragment")]
    DeclaredOriginFragment,
    #[error("declared URL origin must not contain a path")]
    DeclaredOriginPath,
    #[error("URL path literal must be one nonempty segment")]
    InvalidPathLiteral,
    #[error("URL capture name must be nonempty")]
    EmptyCaptureName,
    #[error("URL projection output name {name} is duplicated")]
    DuplicateOutputName { name: String },
    #[error("URL projection output name and query parameter must be nonempty")]
    EmptyQueryName,
    #[error("URL projection input must be a string or null")]
    InputType,
    #[error("URL contains malformed percent encoding")]
    MalformedPercentEncoding,
    #[error("URL path contains invalid UTF-8")]
    InvalidUtf8,
    #[error("URL contains invalid characters")]
    InvalidCharacters,
    #[error("URL must have an explicit HTTP(S) authority")]
    MissingAuthority,
    #[error("URL is invalid")]
    UrlParse(#[source] std::sync::Arc<url::ParseError>),
    #[error("URL path contains a dot segment")]
    DotSegment,
    #[error("URL user information or fragment is not permitted")]
    UserInfoOrFragment,
    #[error("URL origin is not permitted")]
    OriginNotPermitted,
    #[error("URL has no path")]
    MissingPath,
    #[error("URL path does not match the declared shape")]
    PathShapeMismatch,
    #[error("URL identity must be a nonempty path segment")]
    InvalidIdentitySegment,
    #[error("URL contains duplicate selected query parameter {parameter}")]
    DuplicateSelectedQueryParameter { parameter: String },
}

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

fn decode_segment(segment: &str) -> Result<String, UrlProjectionError> {
    let mut result = Vec::with_capacity(segment.len());
    let mut bytes = segment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|b| (b as char).to_digit(16));
            let low = bytes.next().and_then(|b| (b as char).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => result.push((high * 16 + low) as u8),
                _ => return Err(UrlProjectionError::MalformedPercentEncoding),
            }
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).map_err(|_| UrlProjectionError::InvalidUtf8)
}

impl UrlProjection {
    pub fn validate(&self) -> Result<(), UrlProjectionError> {
        if self.origins.is_empty() {
            return Err(UrlProjectionError::NoOrigins);
        }
        for origin in &self.origins {
            let url = Url::parse(origin).map_err(|error| {
                UrlProjectionError::DeclaredOriginParse(std::sync::Arc::new(error))
            })?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(UrlProjectionError::DeclaredOriginScheme);
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(UrlProjectionError::DeclaredOriginUserInfo);
            }
            if url.query().is_some() {
                return Err(UrlProjectionError::DeclaredOriginQuery);
            }
            if url.fragment().is_some() {
                return Err(UrlProjectionError::DeclaredOriginFragment);
            }
            if url.path() != "/" {
                return Err(UrlProjectionError::DeclaredOriginPath);
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
                    return Err(UrlProjectionError::InvalidPathLiteral);
                }
                UrlPathPart::Capture { name } if name.is_empty() => {
                    return Err(UrlProjectionError::EmptyCaptureName);
                }
                UrlPathPart::Capture { name } if !fields.insert(name) => {
                    return Err(UrlProjectionError::DuplicateOutputName { name: name.clone() });
                }
                _ => {}
            }
        }
        for (name, parameter) in &self.query {
            if name.is_empty() || parameter.is_empty() {
                return Err(UrlProjectionError::EmptyQueryName);
            }
            if !fields.insert(name) {
                return Err(UrlProjectionError::DuplicateOutputName { name: name.clone() });
            }
        }
        Ok(())
    }

    pub(crate) fn evaluate(&self, value: Value) -> Result<Value, CmlError> {
        self.validate()?;
        let raw = match value {
            Value::Null => return Ok(Value::Null),
            Value::String(raw) => raw,
            _ => return Err(UrlProjectionError::InputType.into()),
        };
        // Check the input before Url's normalizations can hide malformed escapes or backslashes.
        decode_segment(&raw)?;
        if raw.contains('\\') || raw.chars().any(char::is_control) {
            return Err(UrlProjectionError::InvalidCharacters.into());
        }
        if !raw.starts_with("https://") && !raw.starts_with("http://") {
            return Err(UrlProjectionError::MissingAuthority.into());
        }
        let url = Url::parse(&raw)
            .map_err(|error| UrlProjectionError::UrlParse(std::sync::Arc::new(error)))?;
        // Url normalizes dot segments; reject them before normalization so the declared
        // path contract cannot be satisfied through a different raw resource path.
        if let Some((_, authority_and_path)) = raw.split_once("://") {
            if let Some((_, path)) = authority_and_path.split_once('/') {
                let path = path.split(['?', '#']).next().unwrap_or_default();
                for segment in path.split('/') {
                    if matches!(decode_segment(segment)?.as_str(), "." | "..") {
                        return Err(UrlProjectionError::DotSegment.into());
                    }
                }
            }
        }
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(UrlProjectionError::UserInfoOrFragment.into());
        }
        let origin = url.origin();
        if !self
            .origins
            .iter()
            .any(|allowed| Url::parse(allowed).is_ok_and(|a| a.origin() == origin))
        {
            return Err(UrlProjectionError::OriginNotPermitted.into());
        }
        let segments: Vec<_> = url
            .path_segments()
            .ok_or(UrlProjectionError::MissingPath)?
            .collect();
        if segments.len() != self.path.len() {
            return Err(UrlProjectionError::PathShapeMismatch.into());
        }
        let mut output = IndexMap::new();
        for (segment, part) in segments.into_iter().zip(&self.path) {
            let segment = decode_segment(segment)?;
            if segment.is_empty()
                || segment.contains(['/', '\\'])
                || segment.chars().any(char::is_control)
            {
                return Err(UrlProjectionError::InvalidIdentitySegment.into());
            }
            match part {
                UrlPathPart::Literal { value } if *value != segment => {
                    return Err(UrlProjectionError::PathShapeMismatch.into())
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
                    return Err(UrlProjectionError::DuplicateSelectedQueryParameter {
                        parameter: parameter.clone(),
                    }
                    .into());
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
