//! Import an explicit catalog generation and activate it transactionally.

use clap::Parser;
use plasm_agent::discovery_store::{DiscoveryStore, PreparedCatalog};
use plasm_core::prerequisites::DeploymentBindings;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
enum ImportCatalogsError {
    #[error("read deployment bindings: {0}")]
    ReadBindings(#[source] std::io::Error),
    #[error("decode deployment bindings: {0}")]
    DecodeBindings(#[source] serde_json::Error),
    #[error("load catalog manifest: {0}")]
    LoadCatalog(#[source] plasm_agent::discovery_store::DiscoveryStoreError),
    #[error("set PLASM_DISCOVERY_DATABASE_URL or DATABASE_URL")]
    DatabaseUrlMissing,
    #[error("connect to discovery database: {0}")]
    Connect(#[source] plasm_agent::discovery_store::DiscoveryStoreError),
    #[error("migrate discovery database: {0}")]
    Migrate(#[source] plasm_agent::discovery_store::DiscoveryStoreError),
    #[error("import catalog generation: {0}")]
    Import(#[source] plasm_agent::discovery_store::DiscoveryStoreError),
}

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
async fn main() -> Result<(), ImportCatalogsError> {
    let args = Args::parse();
    let bindings: DeploymentBindings = serde_json::from_slice(
        &std::fs::read(&args.bindings).map_err(ImportCatalogsError::ReadBindings)?,
    )
    .map_err(ImportCatalogsError::DecodeBindings)?;
    let catalogs = args
        .manifest
        .iter()
        .map(|path| PreparedCatalog::load(path).map_err(ImportCatalogsError::LoadCatalog))
        .collect::<Result<Vec<_>, _>>()?;
    let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .map_err(|_| ImportCatalogsError::DatabaseUrlMissing)?;
    let store = DiscoveryStore::connect(&url)
        .await
        .map_err(ImportCatalogsError::Connect)?;
    store
        .migrate()
        .await
        .map_err(ImportCatalogsError::Migrate)?;
    let generation = store
        .import(&args.deployment, catalogs, &bindings)
        .await
        .map_err(ImportCatalogsError::Import)?;
    println!("{generation}");
    Ok(())
}
