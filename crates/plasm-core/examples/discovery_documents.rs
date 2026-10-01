//! Inspect native embedding documents from packed CGS, without provider calls.
//! Usage: cargo run -p plasm-core --example discovery_documents -- catalog.cgs.json ...

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        return Err("supply at least one packed .cgs.json file".into());
    }
    for path in paths {
        let cgs = plasm_core::catalog_il::load_catalog_il_bytes(&std::fs::read(&path)?)?;
        let documents = plasm_core::catalog_discovery::capability_documents(&cgs)?;
        println!("{}", serde_json::to_string(&(path, documents))?);
    }
    Ok(())
}
