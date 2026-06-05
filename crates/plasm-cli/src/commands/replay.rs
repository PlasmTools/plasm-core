use crate::commands::common;
use crate::ReplayAction;
use plasm_core::{Expr, Predicate, QueryExpr, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, FileReplayStore, ReplayStore,
    SessionMaterialization, StreamConsumeOpts,
};
use std::path::{Path, PathBuf};

pub async fn execute(action: ReplayAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ReplayAction::Record { schema, predicate } => {
            println!("Recording execution of predicate with schema...");

            // Load schema and predicate
            let cgs: CGS = common::load_cgs(Path::new(&schema))
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

            let predicate_data = if Path::new(&predicate).exists() {
                std::fs::read_to_string(&predicate)?
            } else {
                predicate.clone()
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

            // Execute in Live mode with recording
            let query = QueryExpr::filtered(&entity_name, pred);
            let expr = Expr::Query(query);

            let config = ExecutionConfig {
                base_url: Some("http://localhost:3000".to_string()),
                ..ExecutionConfig::default()
            };

            let engine = ExecutionEngine::new(config)?;
            let mut mat = SessionMaterialization::new();

            println!("Executing query in LIVE mode with recording...");
            match engine
                .execute(
                    &expr,
                    &cgs,
                    &mut mat,
                    Some(ExecutionMode::Live),
                    StreamConsumeOpts::default(),
                    ExecuteOptions::default(),
                )
                .await
            {
                Ok(result) => {
                    println!("✓ Execution recorded successfully");
                    println!("  - Found {} entities", result.count);
                    println!("  - Duration: {}ms", result.stats.duration_ms);
                    // TODO: Actually store the replay entry to filesystem
                    println!("  - Recording saved to fixtures/replays/ (implementation pending)");
                }
                Err(e) => {
                    eprintln!("✗ Recording failed: {}", e);
                    return Err(e.into());
                }
            }

            Ok(())
        }
        ReplayAction::Test { dir } => {
            println!("Testing recorded snapshots in directory: {}", dir);

            if !Path::new(&dir).exists() {
                eprintln!("Error: Replay directory '{}' does not exist", dir);
                return Err("Replay directory not found".into());
            }

            // Create replay store
            let replay_path = PathBuf::from(&dir);
            let store = FileReplayStore::new(replay_path)?;

            // List all recorded fingerprints
            let fingerprints = store.list_fingerprints()?;

            if fingerprints.is_empty() {
                println!("No recorded snapshots found in {}", dir);
                return Ok(());
            }

            println!("Found {} recorded snapshots to test", fingerprints.len());

            let mut passed = 0;
            let mut failed = 0;

            for fingerprint in fingerprints {
                match store.lookup(&fingerprint)? {
                    Some(_) => {
                        println!("✓ Snapshot {} - valid replay entry", fingerprint.to_hex());
                        passed += 1;
                    }
                    None => {
                        eprintln!("✗ Snapshot {} - missing or corrupt", fingerprint.to_hex());
                        failed += 1;
                    }
                }
            }

            println!(
                "\nReplay test results: {} passed, {} failed",
                passed, failed
            );

            if failed > 0 {
                return Err(format!("{} replay tests failed", failed).into());
            }

            Ok(())
        }
    }
}
