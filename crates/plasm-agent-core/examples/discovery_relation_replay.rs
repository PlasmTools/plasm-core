//! Fixed-candidate diagnostic for the production semantic renderer and Jev contract.
use anyhow::{Context, Result};
use plasm_agent_core::{discovery_matcher, discovery_service::RoutingReceipt};
use plasm_core::catalog_il::CatalogManifest;
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};
const MODEL: &str = "typesafe/jev-1.13-20260917";
#[derive(Deserialize)]
struct Config {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    sources: Vec<String>,
}
#[derive(Debug, thiserror::Error)]
enum ReplayFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
fn read<T: serde::de::DeserializeOwned>(
    path: impl AsRef<Path>,
) -> std::result::Result<T, ReplayFileError> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn save(
    path: impl AsRef<Path>,
    value: &impl serde::Serialize,
) -> std::result::Result<(), ReplayFileError> {
    std::fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).context("mode")?;
    let baseline = Path::new(args.get(2).context("baseline")?);
    let out = Path::new(args.get(3).context("output")?);
    if mode == "prepare" {
        let config: Config = read(baseline.join("config.json"))?;
        let mut catalogs = BTreeMap::new();
        for case in config.cases {
            let mut docs = BTreeMap::new();
            for source in &case.sources {
                if !catalogs.contains_key(source) {
                    let dir = baseline.join("packs").join(source.replace('/', "--"));
                    let manifest: CatalogManifest = read(dir.join("manifest.json"))?;
                    let cgs = plasm_core::catalog_il::load_catalog_il_verified(
                        &std::fs::read(dir.join(manifest.cgs_json))?,
                        &manifest.cgs_hash,
                    )
                    .map_err(anyhow::Error::msg)?;
                    let rendered = plasm_core::catalog_discovery::capability_documents(&cgs)
                        .map_err(anyhow::Error::msg)?;
                    catalogs.insert(source.clone(), (manifest.entry_id, rendered));
                }
                let (entry, rendered) = &catalogs[source];
                for doc in rendered {
                    docs.insert((entry.clone(), doc.capability.clone()), doc.clone());
                }
            }
            let mut receipt: RoutingReceipt =
                read(baseline.join("cells").join(&case.id).join("receipt.json"))?;
            let mut changed = Vec::new();
            for candidate in &mut receipt.retrieval.candidates {
                let document = docs
                    .get(&(
                        candidate.reference.catalog.clone(),
                        candidate.reference.capability.clone(),
                    ))
                    .context("document")?;
                anyhow::ensure!(
                    document.collection == candidate.document.collection,
                    "collection changed"
                );
                if document.operation != candidate.document.operation {
                    changed.push(candidate.reference.clone());
                }
                candidate.document = document.clone();
            }
            let cell = out.join("cells").join(&case.id);
            std::fs::create_dir_all(&cell)?;
            save(cell.join("input.json"), &receipt)?;
            save(cell.join("changed.json"), &changed)?;
            let batches = discovery_matcher::issue_batches(
                MODEL,
                &receipt.intent_provenance,
                &receipt.retrieval,
            )?;
            anyhow::ensure!(batches.len() <= 2, "page limit for {}", case.id);
            save(
                cell.join("requests.json"),
                &batches.iter().map(|b| b.body()).collect::<Vec<_>>(),
            )?;
        }
    } else if mode == "score" {
        let cell = out.join("cells").join(args.get(4).context("case id")?);
        let receipt: RoutingReceipt = read(cell.join("input.json"))?;
        let requests: Vec<String> = read(cell.join("requests.json"))?;
        let batches = discovery_matcher::issue_batches(
            MODEL,
            &receipt.intent_provenance,
            &receipt.retrieval,
        )?;
        anyhow::ensure!(
            requests
                == batches
                    .iter()
                    .map(|b| b.body().to_owned())
                    .collect::<Vec<_>>(),
            "packet changed"
        );
        let mut answers = Vec::new();
        for (index, batch) in batches.iter().enumerate() {
            answers.extend(discovery_matcher::decode_batch(
                batch,
                &std::fs::read_to_string(cell.join(format!("response-{index}.json")))?,
            )?);
        }
        let matching = discovery_matcher::finish_selection(answers, &receipt.retrieval, &batches)?;
        save(cell.join("matching.json"), &matching)?;
        save(
            cell.join("selected.json"),
            &matching.selected(&receipt.retrieval),
        )?;
    } else {
        anyhow::bail!("unknown mode");
    }
    Ok(())
}
