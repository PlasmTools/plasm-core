//! Parsed `{{param:…}}` / `{{sym:entry.Entity}}` workflow templates — UTF-8 safe.

use thiserror::Error;

use super::utf8_text::Utf8Text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BraceSegment {
    Literal(Utf8Text),
    Param { name: String },
    Sym { entry_id: String, entity: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BraceTemplate {
    pub segments: Vec<BraceSegment>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BraceParseError {
    #[error("unclosed template hole at byte {0}")]
    UnclosedHole(usize),
    #[error("empty template hole at byte {0}")]
    EmptyHole(usize),
    #[error("invalid sym reference `{0}` (expected entry_id.Entity)")]
    InvalidSym(String),
}

/// Parse `{{param:name}}` and `{{sym:entry_id.Entity}}` holes into segments.
pub fn parse_brace_template(source: &str) -> Result<BraceTemplate, BraceParseError> {
    let mut segments = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(end_rel) = source[start..].find("}}") else {
                return Err(BraceParseError::UnclosedHole(i));
            };
            let inner = source[start..start + end_rel].trim();
            if inner.is_empty() {
                return Err(BraceParseError::EmptyHole(i));
            }
            let segment = if let Some(name) = inner.strip_prefix("param:") {
                if name.is_empty() {
                    return Err(BraceParseError::EmptyHole(i));
                }
                BraceSegment::Param {
                    name: name.to_string(),
                }
            } else if let Some(sym) = inner.strip_prefix("sym:") {
                let Some((entry_id, entity)) = sym.split_once('.') else {
                    return Err(BraceParseError::InvalidSym(sym.to_string()));
                };
                if entry_id.is_empty() || entity.is_empty() {
                    return Err(BraceParseError::InvalidSym(sym.to_string()));
                }
                BraceSegment::Sym {
                    entry_id: entry_id.to_string(),
                    entity: entity.to_string(),
                }
            } else {
                i = start + end_rel + 2;
                continue;
            };
            if literal_start < i {
                segments.push(BraceSegment::Literal(
                    Utf8Text::from_str(&source[literal_start..i]),
                ));
            }
            segments.push(segment);
            i = start + end_rel + 2;
            literal_start = i;
            continue;
        }
        i += 1;
    }
    if literal_start < bytes.len() {
        segments.push(BraceSegment::Literal(
            Utf8Text::from_str(&source[literal_start..]),
        ));
    }
    Ok(BraceTemplate { segments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_utf8_in_literals() {
        let t = parse_brace_template("Pokémon {{param:q}}").expect("parse");
        assert_eq!(t.segments.len(), 2);
        let lit = match &t.segments[0] {
            BraceSegment::Literal(s) => s.as_str(),
            _ => panic!("expected literal"),
        };
        assert!(lit.contains('é'));
    }

    #[test]
    fn unrecognized_hole_stays_literal() {
        let t = parse_brace_template("{{unknown}}").expect("parse");
        assert_eq!(t.segments.len(), 1);
        assert_eq!(
            match &t.segments[0] {
                BraceSegment::Literal(s) => s.as_str(),
                _ => panic!("expected literal"),
            },
            "{{unknown}}"
        );
    }
}
