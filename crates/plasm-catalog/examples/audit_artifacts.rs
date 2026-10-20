//! Offline artifact audit and recovery of verified, existing embedding vectors.
use plasm_core::catalog_il::{load_catalog_artifact, CatalogManifest};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let directory = PathBuf::from(args.next().ok_or("artifact directory required")?);
    let cache = PathBuf::from(args.next().ok_or("cache directory required")?);
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".manifest.json") {
            let manifest: CatalogManifest = serde_json::from_slice(&std::fs::read(&path)?)?;
            match load_catalog_artifact(&directory, &manifest) {
                Ok(_) => println!("{}: compatible", manifest.entry_id),
                Err(error) => println!("{}: {error:?}", manifest.entry_id),
            }
        }
        if path.to_string_lossy().ends_with(".discovery.json") {
            let artifact = serde_json::from_slice(&std::fs::read(&path)?)?;
            plasm_catalog::embeddings::cache_artifact(&artifact, &cache).await?;
        }
    }
    Ok(())
}
