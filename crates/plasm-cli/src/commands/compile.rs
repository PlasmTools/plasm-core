use crate::commands::{common, CommandError, InputKind};
use plasm_compile::{compile_predicate, compile_query};
use plasm_core::{Predicate, QueryExpr, CGS};
use std::path::Path;

pub async fn execute(schema: &str, predicate: &str) -> Result<(), CommandError> {
    println!("Compiling predicate with schema...");

    // Load schema
    if !Path::new(schema).exists() {
        eprintln!("Error: Schema file '{}' does not exist", schema);
        return Err(CommandError::InputMissing {
            kind: InputKind::Schema,
            path: schema.into(),
        });
    }

    let cgs: CGS = common::load_cgs(Path::new(schema))?;

    // Load predicate
    let predicate_data = if Path::new(predicate).exists() {
        std::fs::read_to_string(predicate)?
    } else {
        predicate.to_string()
    };

    let pred: Predicate = serde_json::from_str(&predicate_data)?;

    // Find compatible entity for this predicate
    let referenced_fields = pred.referenced_fields();
    let referenced_relations = pred.referenced_relations();

    let mut target_entity = None;

    for (entity_name, entity) in &cgs.entities {
        let has_all_fields = referenced_fields
            .iter()
            .all(|field| entity.fields.contains_key(field.as_str()));
        let has_all_relations = referenced_relations
            .iter()
            .all(|relation| entity.relations.contains_key(relation.as_str()));

        if has_all_fields && has_all_relations {
            target_entity = Some((entity_name, entity));
            break;
        }
    }

    let (entity_name, entity) = target_entity.ok_or_else(|| CommandError::NoCompatibleEntity {
        fields: referenced_fields,
        relations: referenced_relations,
    })?;

    println!("Target entity: {}", entity_name);

    // Compile predicate to backend filter
    let backend_filter = compile_predicate(&pred, entity, &cgs)?;

    println!("✓ Compilation successful");
    println!("Backend filter:");
    println!("{}", serde_json::to_string_pretty(&backend_filter)?);

    // Also show what a query would look like
    let query = QueryExpr::filtered(entity_name, pred);
    if let Some(filter) = compile_query(&query, &cgs)? {
        println!("\nCompiled query filter:");
        println!("{}", serde_json::to_string_pretty(&filter)?);
    }

    Ok(())
}
