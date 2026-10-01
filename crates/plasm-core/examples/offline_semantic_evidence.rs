//! Offline only: derive semantic evidence from an exact frozen catalog.
use plasm_core::catalog_discovery::{
    capability_documents, semantic_wire::SemanticCapability, structured::StructuredCapability,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("catalog path required")?;
    let cgs: plasm_core::CGS = serde_json::from_reader(std::fs::File::open(path)?)?;
    let mut result = serde_json::Map::new();
    for doc in capability_documents(&cgs)? {
        let view = StructuredCapability::new(&cgs, &doc.capability)?;
        result.insert(
            doc.capability.clone(),
            serde_json::json!({"document": doc, "structured": SemanticCapability::project(&view)?}),
        );
    }
    serde_json::to_writer(std::io::stdout(), &result)?;
    Ok(())
}
