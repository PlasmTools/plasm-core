//! Verify the production codec against frozen provider experiments, without IO.
use anyhow::{ensure, Context, Result};
use plasm_agent_core::{discovery_matcher as matcher, discovery_service::RoutingReceipt};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
#[derive(Deserialize)]
struct Config {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
}
fn read<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let root = Path::new(args.get(1).context("artifact root")?);
    let config: Config = read(root.join("discovery-integrated-compact-20260923/config.json"))?;
    let mut count = 0;
    for case in config.cases {
        let receipt: RoutingReceipt = read(
            root.join("discovery-relation-meaning-20260923/cells")
                .join(&case.id)
                .join("input.json"),
        )?;
        let dir = root
            .join("discovery-focus-question-20260923/J")
            .join(&case.id);
        let requests: Vec<Value> = read(dir.join("requests.json"))?;
        let issued = matcher::issue_batches(
            "typesafe/jev-1.13-20260917",
            &receipt.intent_provenance,
            &receipt.retrieval,
        )?;
        ensure!(issued.len() == requests.len(), "page count changed");
        let mut answers = Vec::new();
        for (i, batch) in issued.iter().enumerate() {
            ensure!(
                serde_json::from_str::<Value>(batch.body())? == requests[i],
                "semantic wire changed: {}",
                case.id
            );
            let wire: Value = read(dir.join(format!("wire-{i}.json")))?;
            answers.extend(matcher::decode_batch(
                batch,
                wire["raw_response"].as_str().context("response")?,
            )?);
        }
        let result = matcher::finish_selection(answers, &receipt.retrieval, &issued)?;
        // Response decoding must preserve the candidate references in the exact request.
        for selected in result.selected(&receipt.retrieval) {
            ensure!(
                receipt
                    .retrieval
                    .candidates
                    .iter()
                    .any(|c| c.reference == selected),
                "foreign result"
            );
        }
        count += 1;
    }
    println!("{count}/48 semantic packets match the frozen explicit-question arm; native codecs accept every saved response.");
    Ok(())
}
