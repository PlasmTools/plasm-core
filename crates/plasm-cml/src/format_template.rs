use crate::CmlError;
use serde::{Deserialize, Serialize};

/// Format syntax compiled at the catalog decoding boundary. Inserted values are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FormatTemplate {
    source: String,
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Literal(String),
    Slot(String),
}

impl TryFrom<String> for FormatTemplate {
    type Error = CmlError;

    fn try_from(source: String) -> Result<Self, Self::Error> {
        let mut segments = Vec::new();
        let mut remaining = source.as_str();
        while let Some(start) = remaining.find('{') {
            segments.push(Segment::Literal(remaining[..start].to_owned()));
            let suffix = &remaining[start + 1..];
            let end = suffix.find('}').ok_or_else(|| CmlError::InvalidTemplate {
                message: "unclosed format placeholder".into(),
            })?;
            let name = &suffix[..end];
            if name.is_empty() || name.contains('{') {
                return Err(CmlError::InvalidTemplate {
                    message: "empty or nested format placeholder".into(),
                });
            }
            segments.push(Segment::Slot(name.to_owned()));
            remaining = &suffix[end + 1..];
        }
        segments.push(Segment::Literal(remaining.to_owned()));
        Ok(Self { source, segments })
    }
}

impl From<FormatTemplate> for String {
    fn from(template: FormatTemplate) -> Self {
        template.source
    }
}

impl FormatTemplate {
    fn slots(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Slot(name) => Some(name.as_str()),
            Segment::Literal(_) => None,
        })
    }

    pub(crate) fn validate_vars<T>(
        &self,
        vars: &indexmap::IndexMap<String, T>,
    ) -> Result<(), CmlError> {
        for name in self.slots() {
            if !vars.contains_key(name) {
                return Err(CmlError::InvalidTemplate {
                    message: format!("missing format var '{name}'"),
                });
            }
        }
        for name in vars.keys() {
            if !self.slots().any(|slot| slot == name) {
                return Err(CmlError::InvalidTemplate {
                    message: format!("unused format var '{name}'"),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn render(
        &self,
        mut resolve: impl FnMut(&str) -> Result<String, CmlError>,
    ) -> Result<String, CmlError> {
        let mut output = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => output.push_str(text),
                Segment::Slot(name) => output.push_str(&resolve(name)?),
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_source_fails_decoding() {
        for source in ["{", "{}", "{{a}"] {
            assert!(serde_json::from_value::<FormatTemplate>(source.into()).is_err());
        }
    }

    #[test]
    fn inserted_values_are_never_reinterpreted() {
        let template = FormatTemplate::try_from("{a}/{b}/{a}".to_owned()).unwrap();
        let output = template
            .render(|name| {
                Ok(match name {
                    "a" => "{b}".into(),
                    "b" => "resolved".into(),
                    _ => unreachable!(),
                })
            })
            .unwrap();
        assert_eq!(output, "{b}/resolved/{b}");
        assert_eq!(serde_json::to_value(template).unwrap(), "{a}/{b}/{a}");
    }
}
