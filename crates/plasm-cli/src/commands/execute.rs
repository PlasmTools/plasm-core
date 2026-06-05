use crate::commands::common;
use plasm_core::{Expr, Predicate, QueryExpr, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::path::Path;

pub async fn execute(
    schema: &str,
    predicate: &str,
    execution_mode: ExecutionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Executing predicate with schema in {:?} mode...",
        execution_mode
    );

    // Load schema
    if !Path::new(schema).exists() {
        eprintln!("Error: Schema file '{}' does not exist", schema);
        return Err("Schema file not found".into());
    }

    let cgs: CGS = common::load_cgs(Path::new(schema))
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Load predicate
    let predicate_data = if Path::new(predicate).exists() {
        std::fs::read_to_string(predicate)?
    } else {
        predicate.to_string()
    };

    let pred: Predicate = serde_json::from_str(&predicate_data)?;

    // Find target entity
    let referenced_fields = pred.referenced_fields();
    let referenced_relations = pred.referenced_relations();

    let mut target_entity_name = None;

    for (entity_name, entity) in &cgs.entities {
        let has_all_fields = referenced_fields
            .iter()
            .all(|field| entity.fields.contains_key(field.as_str()));
        let has_all_relations = referenced_relations
            .iter()
            .all(|relation| entity.relations.contains_key(relation.as_str()));

        if has_all_fields && has_all_relations {
            target_entity_name = Some(entity_name.clone());
            break;
        }
    }

    let entity_name = target_entity_name.ok_or("No compatible entity found")?;
    println!("Target entity: {}", entity_name);

    // Create query expression
    let query = QueryExpr::filtered(&entity_name, pred);
    let expr = Expr::Query(query);

    // Set up execution engine - point to mock backend for POC
    let config = ExecutionConfig {
        base_url: Some("http://localhost:3000".to_string()),
        ..ExecutionConfig::default()
    };

    let engine = ExecutionEngine::new(config)?;
    let mut mat = SessionMaterialization::new();

    // Execute the query
    println!("Executing query...");

    match engine
        .execute(
            &expr,
            &cgs,
            &mut mat,
            Some(execution_mode),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
    {
        Ok(result) => {
            println!("✓ Execution successful");
            println!("  - Found {} entities", result.count);
            println!("  - Source: {:?}", result.source);
            println!("  - Duration: {}ms", result.stats.duration_ms);
            println!("  - Network requests: {}", result.stats.network_requests);
            println!("  - Cache hits: {}", result.stats.cache_hits);

            if !result.entities.is_empty() {
                println!("\nEntities:");
                for entity in &result.entities {
                    println!("  - {}: {:?}", entity.reference, entity.fields);
                }
            }
        }
        Err(e) => {
            eprintln!("✗ Execution failed: {}", e);

            if execution_mode == ExecutionMode::Live {
                eprintln!("Note: Make sure the mock backend is running on localhost:3000");
                eprintln!("You can start it with: plasm mock --port 3000");
            }

            return Err(e.into());
        }
    }

    Ok(())
}
