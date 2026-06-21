//! Parsed `${ident}` / `${ident.path}` templates with `$$` escape — UTF-8 safe.

use thiserror::Error;

use super::utf8_text::Utf8Text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DollarSegment {
    Literal(Utf8Text),
    Ref { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarTemplate {
    pub segments: Vec<DollarSegment>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DollarParseError {
    #[error("unterminated `${{...}}` substitution at byte {0}")]
    UnterminatedRef(usize),
    #[error("empty `${{...}}` reference at byte {0}")]
    EmptyRef(usize),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InterpolateError {
    #[error("template parse: {0}")]
    Parse(#[from] DollarParseError),
    #[error("unresolved template reference `${path}` (in-scope bindings: {available})")]
    UnresolvedReference { path: String, available: String },
    #[error("interpolated string exceeds maximum length ({max} bytes)")]
    MaxLengthExceeded { max: usize },
}

pub const DEFAULT_MAX_INTERPOLATED_LEN: usize = 512 * 1024;

/// Parse `${…}` holes into literal / ref segments. Literal spans preserve UTF-8 scalar values.
pub fn parse_dollar_template(input: &str) -> Result<DollarTemplate, DollarParseError> {
    let mut segments = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'$' {
                if literal_start < i {
                    segments.push(DollarSegment::Literal(
                        Utf8Text::from_str(&input[literal_start..i]),
                    ));
                }
                segments.push(DollarSegment::Literal(Utf8Text::from_str("$")));
                i += 2;
                literal_start = i;
                continue;
            }
            if bytes[i + 1] == b'{' {
                if literal_start < i {
                    segments.push(DollarSegment::Literal(
                        Utf8Text::from_str(&input[literal_start..i]),
                    ));
                }
                let start = i + 2;
                let Some(end_rel) = input[start..].find('}') else {
                    return Err(DollarParseError::UnterminatedRef(i));
                };
                let path = input[start..start + end_rel].trim();
                if path.is_empty() {
                    return Err(DollarParseError::EmptyRef(i));
                }
                segments.push(DollarSegment::Ref {
                    path: path.to_string(),
                });
                i = start + end_rel + 1;
                literal_start = i;
                continue;
            }
        }
        i += 1;
    }
    if literal_start < bytes.len() {
        segments.push(DollarSegment::Literal(
            Utf8Text::from_str(&input[literal_start..]),
        ));
    }
    Ok(DollarTemplate { segments })
}

impl DollarTemplate {
    pub fn interpolate(
        &self,
        resolve: impl Fn(&str) -> Result<String, String>,
        max_len: usize,
    ) -> Result<Utf8Text, InterpolateError> {
        let mut out = Utf8Text::default();
        for seg in &self.segments {
            match seg {
                DollarSegment::Literal(lit) => {
                    out.push_str(lit.as_str());
                    if out.as_str().len() > max_len {
                        return Err(InterpolateError::MaxLengthExceeded { max: max_len });
                    }
                }
                DollarSegment::Ref { path } => {
                    let value = resolve(path).map_err(|available| {
                        InterpolateError::UnresolvedReference {
                            path: path.clone(),
                            available,
                        }
                    })?;
                    out.push_str(&value);
                    if out.as_str().len() > max_len {
                        return Err(InterpolateError::MaxLengthExceeded { max: max_len });
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Expand `${ident}` / `${ident.path}` without building segment IR (same UTF-8-safe scan).
pub fn interpolate_dollar_template(
    input: &str,
    resolve: impl Fn(&str) -> Result<String, String>,
    max_len: usize,
) -> Result<Utf8Text, InterpolateError> {
    parse_dollar_template(input)?.interpolate(resolve, max_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_utf8_literals() {
        let out = interpolate_dollar_template(
            "Featured Pokémon",
            |_| Ok(String::new()),
            DEFAULT_MAX_INTERPOLATED_LEN,
        )
        .expect("interpolate");
        assert_eq!(out.as_str(), "Featured Pokémon");
        assert!(out.as_str().contains('é'));
    }

    #[test]
    fn preserves_utf8_with_ref() {
        let out = interpolate_dollar_template(
            "# Pokémon\n${body.content}",
            |path| {
                assert_eq!(path, "body.content");
                Ok("electric".into())
            },
            DEFAULT_MAX_INTERPOLATED_LEN,
        )
        .expect("interpolate");
        assert!(out.as_str().starts_with("# Pokémon"));
        assert!(out.as_str().ends_with("electric"));
    }

    #[test]
    fn escape_dollar_dollar() {
        let out = interpolate_dollar_template("cost $$50", |_| Ok(String::new()), usize::MAX)
            .expect("interpolate");
        assert_eq!(out.as_str(), "cost $50");
    }

    #[test]
    fn empty_ref_errors_at_parse() {
        let err = parse_dollar_template("${}").unwrap_err();
        assert!(matches!(err, DollarParseError::EmptyRef(_)));
    }
}
