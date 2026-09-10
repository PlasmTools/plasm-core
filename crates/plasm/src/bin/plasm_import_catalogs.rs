//! Import an explicit catalog generation and activate it transactionally.

use anyhow::{Context, Result};
use clap::Parser;
use plasm_agent::discovery_store::{DiscoveryStore, PreparedCatalog};
use plasm_core::prerequisites::DeploymentBindings;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "plasm-import-catalogs")]
struct Args {
    /// Deployment whose active generation is replaced.
    #[arg(long)]
    deployment: String,
    /// Exact format-3 manifests comprising the complete generation; repeat per catalog.
    #[arg(long, required = true)]
    manifest: Vec<PathBuf>,
    /// Complete deployment bindings, including an explicit empty bindings list when unused.
    #[arg(long)]
    bindings: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let bindings: DeploymentBindings = serde_json::from_slice(&std::fs::read(&args.bindings)?)
        .context("decode deployment bindings")?;
    let catalogs = args
        .manifest
        .iter()
        .map(|path| PreparedCatalog::load(path))
        .collect::<Result<Vec<_>>>()?;
    let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("set PLASM_DISCOVERY_DATABASE_URL or DATABASE_URL")?;
    let store = DiscoveryStore::connect(&url).await?;
    store.migrate().await?;
    let generation = store.import(&args.deployment, catalogs, &bindings).await?;
    println!("{generation}");
    Ok(())
}
