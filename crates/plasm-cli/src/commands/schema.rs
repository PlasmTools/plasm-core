use crate::commands::{common, CommandError, InputKind};
use crate::SchemaAction;
use plasm_core::CGS;
use std::path::Path;

pub async fn execute(action: SchemaAction) -> Result<(), CommandError> {
    match action {
        SchemaAction::Validate { file } => {
            println!("Validating schema file: {}", file);

            if !Path::new(&file).exists() {
                eprintln!("Error: Schema file '{}' does not exist", file);
                return Err(CommandError::InputMissing {
                    kind: InputKind::Schema,
                    path: file.into(),
                });
            }

            let cgs: CGS = common::load_cgs(Path::new(&file))?;

            match cgs.validate() {
                Ok(()) => {
                    println!("✓ Schema validation successful");
                    println!("  - {} entities defined", cgs.entities.len());
                    println!("  - {} capabilities defined", cgs.capabilities.len());

                    for (entity_name, entity) in &cgs.entities {
                        println!(
                            "  - {}: {} fields, {} relations",
                            entity_name,
                            entity.fields.len(),
                            entity.relations.len()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("✗ Schema validation failed: {}", e);
                    return Err(e.into());
                }
            }

            Ok(())
        }
    }
}
