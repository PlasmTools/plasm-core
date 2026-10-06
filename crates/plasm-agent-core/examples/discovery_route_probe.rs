//! Offline packing and live production routing of a frozen diagnostic corpus.
use anyhow::{Context, Result};
use plasm_agent_core::{
    discovery_service::DiscoveryService,
    discovery_store::{DiscoveryAuthorization, DiscoveryStore, PreparedCatalog},
};
use plasm_core::catalog_discovery::{
    content_hash, CapabilityDocument, CatalogDiscoveryArtifact, EmbeddedCapability,
};
use plasm_core::catalog_il::CatalogManifest;
use plasm_core::prerequisites::DeploymentBindings;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Deserialize)]
struct Source {
    source: String,
    manifest: PathBuf,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    intent: String,
    sources: Vec<String>,
}
#[derive(Deserialize)]
struct Config {
    out: PathBuf,
    sources: Vec<Source>,
    cases: Vec<Case>,
    documents: PathBuf,
    vectors: PathBuf,
    bindings: PathBuf,
}
#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Catalog(#[from] plasm_core::catalog_il::CatalogIlError),
    #[error(transparent)]
    Discovery(#[from] plasm_core::catalog_discovery::CatalogDiscoveryError),
    #[error(transparent)]
    Templates(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    Store(#[from] plasm_agent_core::discovery_store::DiscoveryStoreError),
    #[error("manifest path has no containing directory")]
    ManifestDirectoryMissing,
    #[error("frozen document for `{capability}` changed")]
    FrozenDocumentChanged { capability: String },
    #[error("frozen vector for `{capability}` is missing")]
    FrozenVectorMissing { capability: String },
}
fn read<T: serde::de::DeserializeOwned>(p: impl AsRef<Path>) -> std::result::Result<T, ProbeError> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn save(p: impl AsRef<Path>, v: &impl serde::Serialize) -> std::result::Result<(), ProbeError> {
    std::fs::write(p, serde_json::to_vec(v)?)?;
    Ok(())
}
fn destination(c: &Config, s: &str) -> PathBuf {
    c.out.join("packs").join(s.replace('/', "--"))
}
fn prepare(c: &Config) -> std::result::Result<(), ProbeError> {
    let docs: BTreeMap<String, CapabilityDocument> = read(&c.documents)?;
    let vectors: BTreeMap<String, Vec<f32>> = read(&c.vectors)?;
    for source in &c.sources {
        let mut manifest: CatalogManifest = read(&source.manifest)?;
        let root = source
            .manifest
            .parent()
            .ok_or(ProbeError::ManifestDirectoryMissing)?;
        let cgs = plasm_core::catalog_il::load_catalog_il_verified(
            &std::fs::read(root.join(&manifest.cgs_json))?,
            &manifest.cgs_hash,
        )?
        .fresh_catalog_digest();
        let rendered = plasm_core::catalog_discovery::capability_documents(&cgs)?;
        let capabilities = rendered
            .into_iter()
            .map(|document| {
                if docs.get(&format!("{}::{}", source.source, document.capability))
                    != Some(&document)
                {
                    return Err(ProbeError::FrozenDocumentChanged {
                        capability: document.capability.clone(),
                    });
                }
                Ok(EmbeddedCapability {
                    embedding: vectors
                        .get(&document.text)
                        .ok_or_else(|| ProbeError::FrozenVectorMissing {
                            capability: document.capability.clone(),
                        })?
                        .clone(),
                    document,
                })
            })
            .collect::<std::result::Result<Vec<_>, ProbeError>>()?;
        let cgs_bytes = plasm_core::catalog_il::cgs_to_catalog_il_bytes(&cgs)?;
        let recipes = plasm_compile::compile_cgs_capability_templates(&cgs)?;
        let recipe_bytes = serde_json::to_vec(&recipes)?;
        let artifact = CatalogDiscoveryArtifact {
            entry_id: manifest.entry_id.clone(),
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
            profile: Default::default(),
            capabilities,
            prerequisites: cgs.prerequisites.clone(),
        };
        artifact.validate(&cgs)?;
        let discovery_bytes = serde_json::to_vec(&artifact)?;
        manifest.cgs_hash = artifact.cgs_hash;
        manifest.cgs_json = "catalog.cgs.json".into();
        manifest.recipes_json = "recipes.json".into();
        manifest.recipes_hash = content_hash(&recipe_bytes);
        manifest.discovery_json = "discovery.json".into();
        manifest.discovery_hash = content_hash(&discovery_bytes);
        let dir = destination(c, &source.source);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(&manifest.cgs_json), cgs_bytes)?;
        std::fs::write(dir.join(&manifest.recipes_json), recipe_bytes)?;
        std::fs::write(dir.join(&manifest.discovery_json), discovery_bytes)?;
        save(dir.join("manifest.json"), &manifest)?;
        PreparedCatalog::load(&dir.join("manifest.json"))?;
    }
    println!(
        "Prepared {} frozen catalogs with no embedding acquisition",
        c.sources.len()
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let c: Config = read(args.get(2).context("config path required")?)?;
    if args.get(1).map(String::as_str) == Some("prepare") {
        prepare(&c)?;
        return Ok(());
    }
    let case = c
        .cases
        .get(args.get(3).context("case index")?.parse::<usize>()?)
        .context("case")?;
    let store = DiscoveryStore::connect(&std::env::var("PLASM_DISCOVERY_DATABASE_URL")?).await?;
    store.migrate().await?;
    let mut catalogs = Vec::new();
    let mut allowed = BTreeSet::new();
    let mut appworld = BTreeSet::new();
    for source in &case.sources {
        let path = destination(&c, source).join("manifest.json");
        let manifest: CatalogManifest = read(&path)?;
        anyhow::ensure!(
            allowed.insert(manifest.entry_id.clone()),
            "duplicate catalog id in case"
        );
        if source.starts_with("appworld/") {
            appworld.insert(manifest.entry_id);
        }
        catalogs.push(PreparedCatalog::load(&path)?);
    }
    let mut bindings: DeploymentBindings = read(&c.bindings)?;
    bindings
        .bindings
        .retain(|b| appworld.contains(&b.consumer.catalog));
    let generation = store
        .import("integrated-diagnostic", catalogs, &bindings)
        .await?;
    let intent =
        plasm_agent_core::intent_provenance::IntentProvenance::from_turns([case.intent.clone()])
            .map_err(anyhow::Error::msg)?;
    let cell = c.out.join("cells").join(&case.id);
    std::fs::create_dir_all(&cell)?;
    let authorization = DiscoveryAuthorization::catalogs(allowed);
    let retrieval = store
        .retrieve(&generation, &case.intent, &authorization)
        .await?;
    save(cell.join("retrieval.json"), &retrieval)?;
    let service = DiscoveryService::from_env(store)?;
    let result = service
        .route(&generation, &intent, &authorization, &[])
        .await;
    let cell = c.out.join("cells").join(&case.id);
    std::fs::create_dir_all(&cell)?;
    match result {
        Ok(receipt) => save(cell.join("receipt.json"), &receipt)?,
        Err(error) => {
            save(cell.join("error.json"), &format!("{error:#}"))?;
            anyhow::bail!("route failed: {error:#}")
        }
    }
    println!("Resolved {}", case.id);
    Ok(())
}
