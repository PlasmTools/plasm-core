//! Emit CGS as [Mermaid ER diagram](https://mermaid.js.org/syntax/entityRelationshipDiagram.html) text (`erDiagram`).

use crate::commands::common;
use plasm_core::schema::Cardinality;
use plasm_core::{FieldType, CGS};
use std::path::{Path, PathBuf};

/// CLI `--direction` values; rendered as `direction TB` etc. in output.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ErDirection {
    TB,
    BT,
    LR,
    RL,
}

impl ErDirection {
    fn as_mermaid(self) -> &'static str {
        match self {
            ErDirection::TB => "TB",
            ErDirection::BT => "BT",
            ErDirection::LR => "LR",
            ErDirection::RL => "RL",
        }
    }
}

pub struct ErOptions {
    pub relations_only: bool,
    pub direction: Option<ErDirection>,
}

/// Build Mermaid `erDiagram` source from a loaded CGS.
pub fn mermaid_er_from_cgs(cgs: &CGS, options: ErOptions) -> String {
    let mut out = String::from("erDiagram\n");
    if let Some(d) = options.direction {
        out.push_str("    direction ");
        out.push_str(d.as_mermaid());
        out.push('\n');
    }

    for (source_key, entity) in &cgs.entities {
        for rel in entity.relations.values() {
            let source = format_er_entity_token(source_key.as_str());
            let target = format_er_entity_token(rel.target_resource.as_str());
            let connector = match rel.cardinality {
                Cardinality::Many => "||--o{",
                Cardinality::One => "}o--||",
            };
            let label = format_er_label(&rel.name);
            out.push_str("    ");
            out.push_str(&source);
            out.push(' ');
            out.push_str(connector);
            out.push(' ');
            out.push_str(&target);
            out.push_str(" : ");
            out.push_str(&label);
            out.push('\n');
        }
    }

    if !options.relations_only {
        for (entity_key, entity) in &cgs.entities {
            let ename = format_er_entity_token(entity_key.as_str());
            out.push_str("    ");
            out.push_str(&ename);
            out.push_str(" {\n");
            for (fname, field) in &entity.fields {
                let ft = &cgs
                    .named_value_for_slot(field)
                    .expect("entity field value_ref")
                    .field_type;
                let type_str = field_type_mermaid(ft);
                let name_token = if fname == &entity.id_field {
                    format!("*{}", fname)
                } else {
                    fname.to_string()
                };
                out.push_str("        ");
                out.push_str(&type_str);
                out.push(' ');
                out.push_str(&name_token);
                out.push('\n');
            }
            out.push_str("    }\n");
        }
    }

    out
}

fn field_type_mermaid(ft: &FieldType) -> String {
    match ft {
        FieldType::Boolean => "boolean".to_string(),
        FieldType::Number => "float".to_string(),
        FieldType::Integer => "int".to_string(),
        FieldType::String | FieldType::Uuid | FieldType::Select | FieldType::MultiSelect => {
            "string".to_string()
        }
        FieldType::Blob => "blob".to_string(),
        FieldType::Json => "json".to_string(),
        FieldType::Date => "date".to_string(),
        FieldType::Money => "money".to_string(),
        FieldType::Array => "string".to_string(),
        FieldType::EntityRef { target } => {
            format!("ref_{}", sanitize_type_prefix(target.as_str()))
        }
    }
}

/// Mermaid attribute `type` must start with a letter; keep `ref_*` alphanumeric.
fn sanitize_type_prefix(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "Entity".to_string()
    } else if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("T_{}", out)
    } else {
        out
    }
}

/// Entity token: bare identifier, or double-quoted if needed.
fn format_er_entity_token(name: &str) -> String {
    if name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.is_empty() {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "'"))
    }
}

/// Relationship label; quote if not a simple token.
fn format_er_label(name: &str) -> String {
    if name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.is_empty() {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "'"))
    }
}

pub async fn execute(
    schema: &str,
    output: Option<PathBuf>,
    relations_only: bool,
    direction: Option<ErDirection>,
) -> Result<(), Box<dyn std::error::Error>> {
    let cgs = common::load_cgs(Path::new(schema))?;
    let text = mermaid_er_from_cgs(
        &cgs,
        ErOptions {
            relations_only,
            direction,
        },
    );
    if let Some(path) = output {
        tokio::fs::write(path, text).await?;
    } else {
        print!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema;

    #[test]
    fn mermaid_er_from_fixture_includes_diagram_and_relationship() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas");
        let path = dir.join("test_schema.cgs.yaml");
        let cgs = load_schema(&path).expect("load fixture CGS");
        let s = mermaid_er_from_cgs(
            &cgs,
            ErOptions {
                relations_only: true,
                direction: None,
            },
        );
        assert!(s.contains("erDiagram"));
        assert!(s.contains("Account"));
        assert!(s.contains("Contact"));
        assert!(s.contains("||--o{"));
        assert!(s.contains("contacts"));
    }
}
