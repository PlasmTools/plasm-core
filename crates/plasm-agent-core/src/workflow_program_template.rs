//! Structured workflow program template IR — no ad-hoc string replace.

use std::collections::BTreeMap;

use crate::workflow_learning::teaching::{SemanticReference, TeachingError};
use plasm_core::text::{parse_brace_template, BraceParseError, BraceSegment};
use plasm_core::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSegment {
    Literal(String),
    Param { name: String },
    Sym { entry_id: String, entity: String },
    Reference(SemanticReference),
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
    #[error("workflow template contains raw session symbols; use semantic sym holes")]
    RawSessionSymbol,
}

#[derive(Debug, Error)]
pub enum InstantiateError {
    #[error("missing parameter `{0}`")]
    MissingParam(String),
    #[error("unresolved sym `{entry_id}.{entity}`")]
    UnresolvedSym { entry_id: String, entity: String },
    #[error(transparent)]
    Semantic(#[from] TeachingError),
    #[error("parameter cannot be represented as a Python literal: non-finite float")]
    NonFiniteFloat,
    #[error("an expression is not a materialized parameter")]
    UnmaterializedExpression,
    #[error("parameter cannot be encoded as JSON")]
    LiteralJson {
        #[source]
        source: serde_json::Error,
    },
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
            BraceSegment::Literal(lit) => Ok(TemplateSegment::Literal(lit.into_string())),
            BraceSegment::Param { name } => Ok(TemplateSegment::Param { name }),
            BraceSegment::Sym { entry_id, entity } => {
                if let Some((owner, member)) = entity.split_once("::") {
                    let reference = if let Some(capability) = member.strip_prefix("method:") {
                        SemanticReference::Method {
                            entry_id,
                            entity: owner.into(),
                            capability: capability.into(),
                        }
                    } else if let Some(relation) = member.strip_prefix("relation:") {
                        SemanticReference::Relation {
                            entry_id,
                            entity: owner.into(),
                            relation: relation.into(),
                        }
                    } else {
                        return Err(TemplateParseError::InvalidSym(entity));
                    };
                    Ok(TemplateSegment::Reference(reference))
                } else {
                    Ok(TemplateSegment::Sym { entry_id, entity })
                }
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let preview = segments
        .iter()
        .map(|segment| match segment {
            TemplateSegment::Literal(text) => text.clone(),
            _ => "workflow_placeholder".into(),
        })
        .collect::<String>();
    if let Ok(parsed) = ruff_python_parser::parse_module(&preview) {
        use ruff_python_ast::visitor::Visitor;
        struct Raw(bool);
        impl<'a> Visitor<'a> for Raw {
            fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
                let token = match expr {
                    ruff_python_ast::Expr::Name(name) => Some(name.id.as_str()),
                    ruff_python_ast::Expr::Attribute(attr) => Some(attr.attr.as_str()),
                    _ => None,
                };
                self.0 |= token.is_some_and(|token| {
                    token.len() > 1
                        && matches!(token.as_bytes()[0], b'e' | b'm' | b'r')
                        && token.as_bytes()[1..].iter().all(u8::is_ascii_digit)
                });
                ruff_python_ast::visitor::walk_expr(self, expr);
            }
        }
        let mut raw = Raw(false);
        for statement in parsed.suite() {
            raw.visit_stmt(statement);
        }
        if raw.0 {
            return Err(TemplateParseError::RawSessionSymbol);
        }
    }
    Ok(WorkflowProgramTemplate { segments })
}

pub fn instantiate_template(
    template: &WorkflowProgramTemplate,
    params: &BTreeMap<String, Value>,
    resolve: impl Fn(&SemanticReference) -> Result<String, TeachingError>,
) -> Result<String, InstantiateError> {
    let mut out = String::new();
    for seg in &template.segments {
        match seg {
            TemplateSegment::Literal(s) => out.push_str(s),
            TemplateSegment::Param { name } => {
                let v = params
                    .get(name)
                    .ok_or_else(|| InstantiateError::MissingParam(name.clone()))?;
                out.push_str(&render_python_literal(v)?);
            }
            TemplateSegment::Sym { entry_id, entity } => {
                out.push_str(&resolve(&SemanticReference::Entity {
                    entry_id: entry_id.clone(),
                    entity: entity.clone(),
                })?);
            }
            TemplateSegment::Reference(reference) => out.push_str(&resolve(reference)?),
        }
    }
    Ok(out)
}

fn render_python_literal(value: &Value) -> Result<String, InstantiateError> {
    fn render(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::Null => "None".into(),
            serde_json::Value::Bool(true) => "True".into(),
            serde_json::Value::Bool(false) => "False".into(),
            serde_json::Value::String(value) => {
                serde_json::to_string(value).expect("string serialization")
            }
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Array(values) => format!(
                "[{}]",
                values.iter().map(render).collect::<Vec<_>>().join(", ")
            ),
            serde_json::Value::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        serde_json::to_string(key).expect("string serialization"),
                        render(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    fn validate(value: &Value) -> Result<(), InstantiateError> {
        match value {
            Value::Float(number) if !number.is_finite() => Err(InstantiateError::NonFiniteFloat),
            Value::Array(items) => items.iter().try_for_each(validate),
            Value::Object(fields) => fields.values().try_for_each(validate),
            Value::PlasmInputRef(_)
            | Value::GetScalarExtract(_)
            | Value::StringTemplate(_)
            | Value::UnionCtor { .. } => Err(InstantiateError::UnmaterializedExpression),
            _ => Ok(()),
        }
    }
    validate(value)?;
    let json =
        serde_json::to_value(value).map_err(|source| InstantiateError::LiteralJson { source })?;
    Ok(render(&json))
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
        let out = instantiate_template(&t, &params, |reference| {
            assert_eq!(reference.entity_owner(), Some(("a", "E")));
            Ok("e1".into())
        })
        .expect("inst");
        assert!(out.contains("\"hello\""));
        assert!(out.contains("e1"));
    }

    #[test]
    fn multiline_param_is_an_escaped_python_literal() {
        let t = parse_program_template("body = {{param:text}}").expect("parse");
        let mut params = BTreeMap::new();
        params.insert("text".into(), Value::String("line1\nline2".into()));
        let out = instantiate_template(&t, &params, |_| panic!("no references")).expect("inst");
        assert_eq!(out, "body = \"line1\\nline2\"");
        assert!(!out.contains("<<"));
    }

    #[test]
    fn parameters_use_python_boolean_null_and_string_escaping() {
        assert_eq!(render_python_literal(&Value::Bool(true)).unwrap(), "True");
        assert_eq!(render_python_literal(&Value::Bool(false)).unwrap(), "False");
        assert_eq!(render_python_literal(&Value::Null).unwrap(), "None");
        for text in ["a\\b", "\"; effect() #", "\n  body\n", "é\t"] {
            let literal = render_python_literal(&Value::String(text.into())).unwrap();
            assert_eq!(serde_json::from_str::<String>(&literal).unwrap(), text);
            ruff_python_parser::parse_module(&format!("value = {literal}\n"))
                .expect("substitution must remain a single Python literal");
        }
        assert!(matches!(
            render_python_literal(&Value::Float(f64::INFINITY)),
            Err(InstantiateError::NonFiniteFloat)
        ));
        assert!(matches!(
            render_python_literal(&Value::Array(vec![Value::Float(f64::NAN)])),
            Err(InstantiateError::NonFiniteFloat)
        ));
        assert_eq!(
            render_python_literal(&Value::Array(vec![Value::Bool(true), Value::Null])).unwrap(),
            "[True, None]"
        );
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

    #[test]
    fn raw_method_tokens_rejected_and_semantic_methods_parse() {
        assert_eq!(
            parse_program_template("{{sym:a.E}}.m12()"),
            Err(TemplateParseError::RawSessionSymbol)
        );
        let template = parse_program_template("{{sym:a.E}}.{{sym:a.E::method:create}}()").unwrap();
        assert!(
            matches!(&template.segments[2], TemplateSegment::Reference(SemanticReference::Method { capability, .. }) if capability == "create")
        );
        parse_program_template("text = 'm12 e1' # r1\n").expect("strings are not references");
    }

    #[test]
    fn literal_json_failure_preserves_its_source() {
        use std::error::Error;
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = InstantiateError::LiteralJson { source };
        assert!(error.source().unwrap().is::<serde_json::Error>());
    }

    #[test]
    fn unmaterialized_parameter_is_rejected_without_rendering_its_contents() {
        let value = Value::UnionCtor {
            ctor_label: "private-expression".into(),
            ctor_fields: Default::default(),
        };
        let error = render_python_literal(&Value::Array(vec![value])).unwrap_err();
        assert!(matches!(&error, InstantiateError::UnmaterializedExpression));
        assert!(!format!("{error:?} {error}").contains("private-expression"));
    }
}
