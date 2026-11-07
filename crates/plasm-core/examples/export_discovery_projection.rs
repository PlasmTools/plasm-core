//! Export both catalogue and current discovery evidence for offline projection controls.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args().nth(1).ok_or("expected fixture root")?;
    let mut paths = std::fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut rows = Vec::new();
    for path in paths.into_iter().filter(|p| p.is_dir()) {
        let cgs = plasm_core::loader::load_schema_dir(&path)?;
        rows.push(serde_json::json!({"id":path.file_name().unwrap().to_str().unwrap(),
            "catalogue":cgs,"documents":plasm_core::catalog_discovery::capability_documents(&cgs)?}));
    }
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}
