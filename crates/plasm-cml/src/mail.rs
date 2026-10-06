//! Deterministic mail submission codecs. No catalog names, ambient inputs or clocks.

use crate::CmlError;
use indexmap::IndexMap;
use plasm_core::Value;

#[derive(Debug, Clone, thiserror::Error)]
pub enum MailError {
    #[error("mail headers require an object")]
    HeadersType,
    #[error("invalid mail header name")]
    InvalidHeaderName,
    #[error("mail header `{name}` values must be strings")]
    HeaderValueType { name: String },
    #[error("mail headers cannot contain control characters")]
    HeaderControlCharacters,
    #[error("mail header names must be unique ignoring case")]
    DuplicateHeaderName,
    #[error("mail submission requires a nonblank {name} header")]
    RequiredHeaderMissing { name: &'static str },
    #[error("mail text must be a string")]
    TextType,
    #[error("plain-text mail codec owns MIME content headers")]
    MimeHeaderOverride,
    #[error("mail header exceeds the supported line length ({actual} bytes; maximum {maximum})")]
    HeaderLineTooLong { actual: usize, maximum: usize },
    #[error("reply requires a recipient or parent Reply-To/From")]
    ReplyRecipientMissing,
    #[error("reply requires a parent Message-ID")]
    ReplyMessageIdMissing,
}

fn headers(value: Value) -> Result<IndexMap<String, String>, CmlError> {
    let Value::Object(fields) = value else {
        return Err(MailError::HeadersType.into());
    };
    let mut result = IndexMap::new();
    for (name, value) in fields {
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err(MailError::InvalidHeaderName.into());
        }
        let name = name.to_ascii_lowercase();
        let value = match value {
            Value::Null => continue,
            Value::String(value) => value,
            _ => return Err(MailError::HeaderValueType { name }.into()),
        };
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(MailError::HeaderControlCharacters.into());
        }
        if result.insert(name, value).is_some() {
            return Err(MailError::DuplicateHeaderName.into());
        }
    }
    Ok(result)
}

pub(crate) fn serialize_message(header_values: Value, text: Value) -> Result<Value, CmlError> {
    let headers = headers(header_values)?;
    for name in ["from", "to"] {
        if headers
            .get(name)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(MailError::RequiredHeaderMissing { name }.into());
        }
    }
    let Value::String(text) = text else {
        return Err(MailError::TextType.into());
    };
    let mut lines = Vec::new();
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "mime-version" | "content-type" | "content-transfer-encoding"
        ) {
            return Err(MailError::MimeHeaderOverride.into());
        }
        // UTF-8 submission headers are preserved; the codec does not invent mailbox parsing
        // or encoded-word rules. The provider's submission endpoint accepts these headers.
        let line = format!("{name}: {value}");
        if line.len() > 998 {
            return Err(MailError::HeaderLineTooLong {
                actual: line.len(),
                maximum: 998,
            }
            .into());
        }
        lines.push(line);
    }
    lines.extend([
        "MIME-Version: 1.0".into(),
        "Content-Type: text/plain; charset=\"UTF-8\"".into(),
        "Content-Transfer-Encoding: base64".into(),
    ]);
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(normalized);
    let encoded = encoded
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).expect("base64 is ASCII"))
        .collect::<Vec<_>>()
        .join("\r\n");
    Ok(Value::String(format!(
        "{}\r\n\r\n{encoded}\r\n",
        lines.join("\r\n")
    )))
}

pub(crate) fn reply_headers(parent: Value, overrides: Value) -> Result<Value, CmlError> {
    let parent = headers(parent)?;
    let mut result = headers(overrides)?;
    let get = |name: &str| parent.get(name).filter(|v| !v.trim().is_empty());
    if result.get("to").is_none_or(|v| v.trim().is_empty()) {
        let to = get("reply-to")
            .or_else(|| get("from"))
            .ok_or(MailError::ReplyRecipientMissing)?;
        // Preserve the header's mailbox list and display names. Do not guess a first address.
        result.insert("to".into(), to.clone());
    }
    if result.get("subject").is_none_or(|v| v.trim().is_empty()) {
        let subject = get("subject").map(|v| v.trim()).unwrap_or_default();
        let subject = if subject
            .get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case("re:"))
        {
            subject.to_string()
        } else if subject.is_empty() {
            "Re:".into()
        } else {
            format!("Re: {subject}")
        };
        result.insert("subject".into(), subject);
    }
    let message_id = get("message-id").ok_or(MailError::ReplyMessageIdMissing)?;
    result
        .entry("in-reply-to".into())
        .or_insert_with(|| message_id.clone());
    result
        .entry("references".into())
        .or_insert_with(|| match get("references") {
            Some(references) => format!("{} {}", references.trim(), message_id.trim()),
            None => message_id.clone(),
        });
    Ok(Value::Object(
        result
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;

    fn value(json: serde_json::Value) -> Value {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn submission_is_deterministic_and_has_no_ambient_headers() {
        let headers =
            value(json!({"From":"a@example.test","To":"b@example.test","Subject":"Hello"}));
        let body = Value::String("Line 1\nLine 2\rLine 3\r\n✓".into());
        let first = serialize_message(headers.clone(), body.clone()).unwrap();
        assert_eq!(first, serialize_message(headers, body).unwrap());
        let Value::String(wire) = first else {
            panic!("mail string");
        };
        let (headers, text) = wire.split_once("\r\n\r\n").unwrap();
        assert!(!headers.to_lowercase().contains("message-id:"));
        assert!(!headers.to_lowercase().contains("date:"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(text.replace("\r\n", ""))
            .unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            "Line 1\r\nLine 2\r\nLine 3\r\n✓"
        );
    }

    #[test]
    fn reply_fields_are_explicit_and_preserve_mailboxes() {
        let result = reply_headers(value(json!({"From":"Sender <sender@example.test>","Reply-To":"Team <team@example.test>","Subject":"Question","Message-ID":"<parent@example.test>","References":"<older@example.test>"})), value(json!({"From":"me@example.test"}))).unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"from":"me@example.test","to":"Team <team@example.test>","subject":"Re: Question","in-reply-to":"<parent@example.test>","references":"<older@example.test> <parent@example.test>"})
        );
        assert!(reply_headers(value(json!({})), value(json!({"To":"b@example.test"}))).is_err());
    }

    #[test]
    fn invalid_headers_fail_without_echoing_contents() {
        for (headers, expected) in [
            (
                json!({"From":"a","To":"b","Subject":"private\r\nInjected: x"}),
                MailError::HeaderControlCharacters,
            ),
            (
                json!({"From":"a","To":"b","subject":"a","Subject":"b"}),
                MailError::DuplicateHeaderName,
            ),
            (
                json!({"From":"a","To":"b","Content-Type":"text/html"}),
                MailError::MimeHeaderOverride,
            ),
            (
                json!({"From":"a","To":"b","Subject":false}),
                MailError::HeaderValueType {
                    name: "subject".into(),
                },
            ),
        ] {
            let error =
                serialize_message(value(headers), Value::String("body".into())).unwrap_err();
            let CmlError::Mail(actual) = &error else {
                panic!("expected mail rejection: {error:?}");
            };
            assert_eq!(
                std::mem::discriminant(actual),
                std::mem::discriminant(&expected)
            );
            if let MailError::HeaderValueType { name } = actual {
                assert_eq!(name, "subject");
            }
            assert!(!error.to_string().contains("private"));
        }
    }
}
