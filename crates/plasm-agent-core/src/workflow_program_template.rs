//! Structured workflow program template IR — no ad-hoc string replace.

use std::collections::BTreeMap;

use plasm_core::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSegment {
    Literal(String),
    Param { name: String },
    Sym { entry_id: String, entity: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowProgramTemplate {
    pub segments: Vec<TemplateSegment>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemplateParseError {
    #[error("unclosed template hole at byte {0}")]
    UnclosedHole(usize),
    #[error("empty template hole at byte {0}")]
    EmptyHole(usize),
    #[error("invalid sym reference `{0}` (expected entry_id.Entity)")]
    InvalidSym(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InstantiateError {
    #[error("missing parameter `{0}`")]
    MissingParam(String),
    #[error("unresolved sym `{entry_id}.{entity}`")]
    UnresolvedSym { entry_id: String, entity: String },
    #[error("template parse: {0}")]
    Parse(#[from] TemplateParseError),
}

/// Parse `{{param:name}}` and `{{sym:entry_id.Entity}}` holes into segments.
pub fn parse_program_template(source: &str) -> Result<WorkflowProgramTemplate, TemplateParseError> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if !literal.is_empty() {
                segments.push(TemplateSegment::Literal(std::mem::take(&mut literal)));
            }
            let start = i + 2;
            let Some(end_rel) = source[start..].find("}}") else {
                return Err(TemplateParseError::UnclosedHole(i));
            };
            let inner = source[start..start + end_rel].trim();
            if inner.is_empty() {
                return Err(TemplateParseError::EmptyHole(i));
            }
            let segment = if let Some(name) = inner.strip_prefix("param:") {
                if name.is_empty() {
                    return Err(TemplateParseError::EmptyHole(i));
                }
                TemplateSegment::Param {
                    name: name.to_string(),
                }
            } else if let Some(sym) = inner.strip_prefix("sym:") {
                let Some((entry_id, entity)) = sym.split_once('.') else {
                    return Err(TemplateParseError::InvalidSym(sym.to_string()));
                };
                if entry_id.is_empty() || entity.is_empty() {
                    return Err(TemplateParseError::InvalidSym(sym.to_string()));
                }
                TemplateSegment::Sym {
                    entry_id: entry_id.to_string(),
                    entity: entity.to_string(),
                }
            } else {
                literal.push_str("{{");
                literal.push_str(inner);
                literal.push_str("}}");
                i = start + end_rel + 2;
                continue;
            };
            segments.push(segment);
            i = start + end_rel + 2;
            continue;
        }
        literal.push(char::from(bytes[i]));
        i += 1;
    }
    if !literal.is_empty() {
        segments.push(TemplateSegment::Literal(literal));
    }
    Ok(WorkflowProgramTemplate { segments })
}

/// Exposure map: `(entry_id, entity)` → teaching symbol like `e1`.
pub type SymExposureMap<'a> = BTreeMap<(String, String), &'a str>;

pub fn instantiate_template(
    template: &WorkflowProgramTemplate,
    params: &BTreeMap<String, Value>,
    exposure: &SymExposureMap<'_>,
) -> Result<String, InstantiateError> {
    let mut out = String::new();
    for seg in &template.segments {
        match seg {
            TemplateSegment::Literal(s) => out.push_str(s),
            TemplateSegment::Param { name } => {
                let v = params
                    .get(name)
                    .ok_or_else(|| InstantiateError::MissingParam(name.clone()))?;
                out.push_str(&render_plasm_scalar(v)?);
            }
            TemplateSegment::Sym { entry_id, entity } => {
                let sym = exposure
                    .get(&(entry_id.clone(), entity.clone()))
                    .ok_or_else(|| InstantiateError::UnresolvedSym {
                        entry_id: entry_id.clone(),
                        entity: entity.clone(),
                    })?;
                out.push_str(sym);
            }
        }
    }
    Ok(out)
}

fn render_plasm_scalar(v: &Value) -> Result<String, InstantiateError> {
    Ok(match v {
        Value::String(s) => {
            if s.contains('\n') || s.contains('"') {
                let tag = format!("PLASM_WF_{:x}", hash_tag(s));
                format!("<<{tag}\n{s}\n{tag}")
            } else {
                format!("\"{s}\"")
            }
        }
        Value::Integer(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "\"\"".into()),
    })
}

fn hash_tag(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_param_and_sym_holes() {
        let t =
            parse_program_template("items = {{sym:catalog_a.WorkItem}}\nfilter = {{param:query}}")
                .expect("parse");
        assert_eq!(t.segments.len(), 4);
    }

    #[test]
    fn instantiate_param_and_sym() {
        let t = parse_program_template("x = {{param:q}}\ny = {{sym:a.E}}").expect("parse");
        let mut params = BTreeMap::new();
        params.insert("q".into(), Value::String("hello".into()));
        let mut exposure = BTreeMap::new();
        exposure.insert(("a".into(), "E".into()), "e1");
        let out = instantiate_template(&t, &params, &exposure).expect("inst");
        assert!(out.contains("\"hello\""));
        assert!(out.contains("e1"));
    }

    #[test]
    fn multiline_param_uses_heredoc() {
        let t = parse_program_template("body = {{param:text}}").expect("parse");
        let mut params = BTreeMap::new();
        params.insert("text".into(), Value::String("line1\nline2".into()));
        let out = instantiate_template(&t, &params, &BTreeMap::new()).expect("inst");
        assert!(out.contains("<<PLASM_WF_"));
        assert!(out.contains("line1\nline2"));
    }
}
