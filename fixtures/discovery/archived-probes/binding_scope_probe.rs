//! Archived research source; not compatible with the current discovery API.
use plasm_agent_core::{
    discovery_matcher::{evidence::CatalogEvidence, issue_batches, RELEVANCE_MODEL},
    discovery_store::{RetrievalReceipt, RetrievedCapability},
    intent_provenance::IntentProvenance,
};
use plasm_core::prerequisites::{CapabilityRef, DeploymentBindings, prerequisite_closure, project_input_source_candidates};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Job { id: String, catalogs: BTreeMap<String,String>, intent_turns: Vec<String>, references: Option<Vec<CapabilityRef>>, bindings: Option<String> }

fn main() -> Result<(),Box<dyn std::error::Error>> {
    let jobs: Vec<Job> = serde_json::from_reader(std::io::stdin())?;
    let mut out=Vec::new();
    for job in jobs {
        let catalogs: BTreeMap<String,plasm_core::CGS> = job.catalogs.iter().map(|(id,path)| {
            let path=std::path::Path::new(path);
            let cgs=if path.is_dir() { plasm_core::loader::load_schema_dir(path)? } else { serde_json::from_slice(&std::fs::read(path)?)? };
            Ok((id.clone(),cgs))
        }).collect::<Result<_,Box<dyn std::error::Error>>>()?;
        let mut candidates=Vec::new();
        for (catalog,cgs) in &catalogs {
            for document in plasm_core::catalog_discovery::capability_documents(cgs)? {
                let reference=CapabilityRef{catalog:catalog.clone(),capability:document.capability.clone()};
                if job.references.as_ref().is_some_and(|r| !r.contains(&reference)) {continue;}
                candidates.push(RetrievedCapability{id:format!("c{}",candidates.len()),reference,document,admissions:BTreeSet::from(["lexical".into()])});
            }
        }
        let refs:Vec<_>=candidates.iter().map(|c|c.reference.clone()).collect();
        if let Some(wanted)=&job.references { assert_eq!(wanted.len(),refs.len()); }
        let permitted:BTreeSet<_>=refs.iter().cloned().collect();
        let borrowed=catalogs.iter().map(|(k,v)|(k.clone(),v)).collect();
        let sources=project_input_source_candidates(&borrowed,&refs,&|r|permitted.contains(r))?;
        let bindings=match job.bindings {Some(path)=>serde_json::from_slice(&std::fs::read(path)?)?,None=>DeploymentBindings::default()};
        let closure=prerequisite_closure(&borrowed,&bindings,&refs,&catalogs.keys().cloned().collect())?;
        let context: BTreeSet<_>=closure.edges.iter().flat_map(|e|[e.consumer.clone(),e.provider.clone()]).filter(|r|!permitted.contains(r)).collect();
        for reference in &context {
            let cgs=&catalogs[&reference.catalog];
            let document=plasm_core::catalog_discovery::capability_documents(cgs)?.into_iter().find(|d|d.capability==reference.capability).ok_or("missing prerequisite document")?;
            candidates.push(RetrievedCapability{id:format!("c{}",candidates.len()),reference:reference.clone(),document,admissions:BTreeSet::from(["prerequisite-context".into()])});
        }
        let retrieval=RetrievalReceipt{generation:"binding-scope-research".into(),lexical_count:refs.len(),vector_count:0,lexical_truncated:false,vector_truncated:false,candidates};
        let evidence=CatalogEvidence::from_catalogs(&retrieval,&catalogs)?;
        let provenance=IntentProvenance::from_turns(job.intent_turns)?;
        let batches=issue_batches(RELEVANCE_MODEL,&provenance,&retrieval,&evidence)?;
        let mut bodies:Vec<serde_json::Value>=batches.iter().map(|b|serde_json::from_str(b.body())).collect::<Result<_,_>>()?;
        for body in &mut bodies {
            let cards=body["state"]["capabilities"].clone();
            body["questions"].as_object_mut().ok_or("missing questions")?.retain(|q,_| {
                let key=format!("c{}",&q[1..]);
                serde_json::from_value::<CapabilityRef>(cards[&key]["capability"]["reference"].clone()).is_ok_and(|r|permitted.contains(&r))
            });
        }
        bodies.retain(|b|b["questions"].as_object().is_some_and(|q|!q.is_empty()));
        out.push(json!({"id":job.id,"bodies":bodies,"sources":sources,"prerequisites":closure.edges,"supplemental_prerequisite_context":context}));
    }
    serde_json::to_writer(std::io::stdout(),&out)?;
    Ok(())
}
