//! Diagnostic-only adapter; no network, selection changes, or benchmark instructions.
use plasm_agent_core::{
    discovery_matcher::{decode_batch, issue_batches, RELEVANCE_MODEL},
    discovery_store::{RetrievalReceipt, RetrievedCapability},
    intent_provenance::IntentProvenance,
};
use serde::Deserialize;
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    catalogs: std::collections::BTreeMap<String, plasm_core::CGS>,
    intent_turns: Vec<String>,
    candidates: Vec<RetrievedCapability>,
    responses: Option<Vec<String>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    let input: Input = serde_json::from_str(&text)?;
    let provenance = IntentProvenance::from_turns(input.intent_turns)?;
    let receipt = RetrievalReceipt {
        generation: "offline-diagnostic".into(),
        lexical_count: input.candidates.len(),
        vector_count: 0,
        lexical_truncated: false,
        vector_truncated: false,
        candidates: input.candidates,
    };
    let evidence = plasm_agent_core::discovery_matcher::evidence::CatalogEvidence::from_catalogs(
        &receipt,
        &input.catalogs,
        &Default::default(),
        &|_| true,
    )?;
    let batches = issue_batches(RELEVANCE_MODEL, &provenance, &receipt, &evidence)?;
    if let Some(responses) = input.responses {
        if responses.len() != batches.len() {
            return Err("response count differs from issued page count".into());
        }
        for (batch, raw) in batches.iter().zip(responses) {
            decode_batch(batch, &raw)?;
        }
    }
    let output: Vec<_> = batches
        .iter()
        .map(|b| {
            serde_json::json!({
                "cache_key": b.cache_key(), "body": b.body()
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
