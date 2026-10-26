//! Offline experiment bridge to the production relevance codec. No network access.
use anyhow::{ensure, Result};
use plasm_agent_core::{
    discovery_matcher::{prepare_selection, SelectionPages, RELEVANCE_MODEL},
    discovery_store::RetrievalReceipt,
    intent_provenance::IntentProvenance,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    catalogs: std::collections::BTreeMap<String, plasm_core::CGS>,
    intent: String,
    retrieval: RetrievalReceipt,
    #[serde(default)]
    responses: Option<Vec<String>>,
}
fn main() -> Result<()> {
    let input: Input = serde_json::from_reader(std::io::stdin())?;
    let provenance = IntentProvenance::from_turns([input.intent])?;
    let evidence = plasm_agent_core::discovery_matcher::evidence::CatalogEvidence::from_catalogs(
        &input.retrieval,
        &input.catalogs,
        &Default::default(),
        &|_| true,
    )?;
    let prepared = prepare_selection(RELEVANCE_MODEL, &provenance, &input.retrieval, &evidence)?;
    if let Some(responses) = input.responses {
        ensure!(
            responses.len() == prepared.batches().len(),
            "page count mismatch"
        );
        let mut pages = SelectionPages::from_prepared(prepared);
        for response in responses {
            pages.accept(&response)?;
        }
        let receipt = pages.finish(&input.retrieval)?;
        serde_json::to_writer(std::io::stdout(), &receipt)?;
    } else {
        let bodies: Vec<serde_json::Value> = prepared
            .batches()
            .iter()
            .map(|batch| serde_json::from_str(batch.body()))
            .collect::<Result<_, _>>()?;
        serde_json::to_writer(
            std::io::stdout(),
            &serde_json::json!({"requests":bodies,"budget_retained":prepared.budget_retained()}),
        )?;
    }
    Ok(())
}
