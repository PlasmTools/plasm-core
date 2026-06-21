//! Structured workflow program template IR — no ad-hoc string replace.

use std::collections::BTreeMap;

use plasm_core::text::{parse_brace_template, BraceParseError, BraceSegment};
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

fn map_brace_parse_error(err: BraceParseError) -> TemplateParseError {
    match err {
        BraceParseError::UnclosedHole(i) => TemplateParseError::UnclosedHole(i),
        BraceParseError::EmptyHole(i) => TemplateParseError::EmptyHole(i),
        BraceParseError::InvalidSym(s) => TemplateParseError::InvalidSym(s),
    }
}

/// Parse `{{param:name}}` and `{{sym:entry_id.Entity}}` holes into segments.
pub fn parse_program_template(source: &str) -> Result<WorkflowProgramTemplate, TemplateParseError> {
    let parsed = parse_brace_template(source).map_err(map_brace_parse_error)?;
    let segments = parsed
        .segments
        .into_iter()
        .map(|seg| match seg {
            BraceSegment::Literal(lit) => TemplateSegment::Literal(lit.into_string()),
            BraceSegment::Param { name } => TemplateSegment::Param { name },
            BraceSegment::Sym { entry_id, entity } => TemplateSegment::Sym {
                entry_id,
                entity,
            },
        })
        .collect();
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

    #[test]
    fn preserves_utf8_in_literals() {
        let t = parse_program_template("Pokémon {{param:q}}").expect("parse");
        let lit = match &t.segments[0] {
            TemplateSegment::Literal(s) => s,
            _ => panic!("expected literal"),
        };
        assert!(lit.contains('é'));
    }
}
