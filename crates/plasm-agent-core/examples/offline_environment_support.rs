//! Offline adapter to the integrated support request/decoder; no provider IO.
use anyhow::Result;
use plasm_agent_core::{
    discovery_matcher::JEV_MODEL, discovery_support, intent_provenance::IntentProvenance,
};
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Deserialize)]
struct Input {
    schema: std::path::PathBuf,
    catalog: String,
    intent: String,
    available: Vec<String>,
    response: Option<String>,
}
fn main() -> Result<()> {
    let input: Input = serde_json::from_reader(std::io::stdin())?;
    let mut cgs = plasm_core::load_schema(&input.schema)?;
    cgs.entry_id = Some(input.catalog);
    let catalog = cgs
        .entry_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("catalog identity missing"))?;
    let available: BTreeSet<_> = input
        .available
        .into_iter()
        .map(|capability| CapabilityRef {
            catalog: catalog.clone(),
            capability,
        })
        .collect();
    let intent = IntentProvenance::from_turns([input.intent])?;
    let issued = discovery_support::issue(
        JEV_MODEL,
        &intent,
        &available,
        &BTreeMap::from([(catalog, cgs)]),
    )?;
    if let Some(raw) = input.response {
        serde_json::to_writer(
            std::io::stdout(),
            &discovery_support::decode(&issued, &raw)?,
        )?;
    } else {
        print!("{}", issued.body);
    }
    Ok(())
}
