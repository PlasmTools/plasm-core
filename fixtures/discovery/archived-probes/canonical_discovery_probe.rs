//! Archived research source; not compatible with the current discovery API.
use plasm_agent_core::{
    discovery_matcher::{evidence::CatalogEvidence, issue_batches, RELEVANCE_MODEL},
    discovery_store::{RetrievalReceipt, RetrievedCapability},
    intent_provenance::IntentProvenance,
};
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Job {
    id: String,
    catalogs: BTreeMap<String, String>,
    intent_turns: Vec<String>,
    references: Option<Vec<CapabilityRef>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let jobs: Vec<Job> = serde_json::from_reader(std::io::stdin())?;
    let mut output = Vec::new();
    for job in jobs {
        let catalogs: BTreeMap<String, plasm_core::CGS> = job.catalogs.iter().map(|(name, path)| {
            let path = std::path::Path::new(path);
            let cgs = if path.is_dir() {
                plasm_core::loader::load_schema_dir(path)?
            } else {
                serde_json::from_slice(&std::fs::read(path)?)?
            };
            Ok((name.clone(), cgs))
        }).collect::<Result<_, Box<dyn std::error::Error>>>()?;
        let mut candidates = Vec::new();
        for (catalog, cgs) in &catalogs {
            for document in plasm_core::catalog_discovery::capability_documents(cgs)? {
                let reference = CapabilityRef { catalog: catalog.clone(), capability: document.capability.clone() };
                if job.references.as_ref().is_some_and(|refs| !refs.contains(&reference)) { continue; }
                candidates.push(RetrievedCapability {
                    id: format!("candidate{}", candidates.len()), reference, document,
                    admissions: BTreeSet::from(["lexical".into()]),
                });
            }
        }
        if let Some(refs) = &job.references { assert_eq!(refs.len(), candidates.len()); }
        let retrieval = RetrievalReceipt {
            generation: "canonical-evidence-proof".into(), lexical_count: candidates.len(), vector_count: 0,
            lexical_truncated: false, vector_truncated: false, candidates,
        };
        let evidence = CatalogEvidence::from_catalogs(&retrieval, &catalogs)?;
        let provenance = IntentProvenance::from_turns(job.intent_turns)?;
        let batches = issue_batches(RELEVANCE_MODEL, &provenance, &retrieval, &evidence)?;
        let mut pairs = Vec::new();
        for batch in batches {
            let canonical: Value = serde_json::from_str(batch.body())?;
            let mut previous = canonical.clone();
            for card in previous["state"]["capabilities"].as_object_mut().ok_or("missing cards")?.values_mut() {
                let reference: CapabilityRef = serde_json::from_value(card["capability"]["reference"].clone())?;
                let document = &retrieval.candidates.iter().find(|c| c.reference == reference).ok_or("missing candidate")?.document;
                card["capability"]["operation"] = json!(document.operation);
                card["collection_meaning"] = json!(document.collection.meaning);
                let declaration = card["declaration"].as_object_mut().ok_or("missing declaration")?;
                for key in ["acquired_inputs", "relation_uses", "input_sources"] { declaration.remove(key); }
            }
            pairs.push(json!({"previous": previous, "canonical": canonical}));
        }
        output.push(json!({"id": job.id, "pairs":pairs}));
    }
    serde_json::to_writer(std::io::stdout(), &output)?;
    Ok(())
}
