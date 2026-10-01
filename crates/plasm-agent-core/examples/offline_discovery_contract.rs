//! Offline experiment bridge to the production relevance codec. No network access.
use anyhow::{ensure, Result};
use plasm_agent_core::{
    discovery_matcher::{issue_batches, SelectionPages, JEV_MODEL},
    discovery_store::RetrievalReceipt,
    intent_provenance::IntentProvenance,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    intent: String,
    retrieval: RetrievalReceipt,
    #[serde(default)]
    responses: Option<Vec<String>>,
}
fn main() -> Result<()> {
    let input: Input = serde_json::from_reader(std::io::stdin())?;
    let provenance = IntentProvenance::from_turns([input.intent])?;
    let batches = issue_batches(JEV_MODEL, &provenance, &input.retrieval)?;
    if let Some(responses) = input.responses {
        ensure!(responses.len() == batches.len(), "page count mismatch");
        let mut pages = SelectionPages::new(batches);
        for response in responses {
            pages.accept(&response)?;
        }
        let receipt = pages.finish(&input.retrieval)?;
        serde_json::to_writer(std::io::stdout(), &receipt)?;
    } else {
        let bodies: Vec<serde_json::Value> = batches
            .iter()
            .map(|batch| serde_json::from_str(batch.body()))
            .collect::<Result<_, _>>()?;
        serde_json::to_writer(std::io::stdout(), &bodies)?;
    }
    Ok(())
}
