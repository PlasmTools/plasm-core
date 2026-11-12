//! Fixed-packet semantic diagnostic; target selection is deliberately non-scoring.
use super::*;
use std::{io::Write, path::PathBuf};

#[path = "connection_diagnostic.rs"]
mod connection_diagnostic;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    pair: String,
    family: String,
    request: String,
    targets: Vec<String>,
    witness: bool,
}

fn reference(capability: &str) -> CapabilityRef {
    CapabilityRef {
        catalog: "diagnostic".into(),
        capability: capability.into(),
    }
}

fn prepared(case: &Case, root: &std::path::Path) -> (IssuedBatch, String) {
    let mut cgs = plasm_core::loader::load_schema_dir(
        &root.join("schemas/discovery_decision_v1").join(&case.id),
    )
    .unwrap();
    cgs.bind_registry_entry_id("diagnostic");
    for cap in cgs.capabilities.values_mut() {
        if cap.inputs.arguments.is_none() {
            cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
        }
    }
    let catalogs = BTreeMap::from([("diagnostic".into(), cgs)]);
    let deployments = DeploymentBindings::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["diagnostic".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns([case.request.clone()]).unwrap();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "decision-interface-v1",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &Default::default(),
    };
    let mut batches = Vec::new();
    for target in &case.targets {
        let target = reference(target);
        let route = if case.witness {
            vec![index
                .graph
                .edges()
                .find(|e| {
                    e.source == target
                        && e.consumer == reference("op_b")
                        && e.relationship == Relationship::SuppliesInput
                })
                .expect("authored typed witness")
                .clone()]
        } else {
            Vec::new()
        };
        let (local, _) = packet(&index, &target, &route, context).unwrap();
        batches.push(local);
    }
    let request = request_packing::connected(batches).unwrap().unwrap();
    assert!(request.body().len() <= search_budget::MAX_INPUT_BYTES);
    let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
    assert_eq!(request.body(), serde_json::to_string(&body).unwrap());
    for (qid, local) in body["state"]["question_evidence"].as_object().unwrap() {
        let target = &index
            .candidates
            .values()
            .find(|c| c.id == request.bindings[qid])
            .unwrap()
            .reference;
        let mut found = false;
        for card in local["capabilities"].as_object().unwrap().values() {
            let r: CapabilityRef =
                serde_json::from_value(card["capability"]["reference"].clone()).unwrap();
            assert!(allowed.permits(&r));
            if &r == target {
                let (_, canonical) = index
                    .evidence(
                        "decision-interface-v1",
                        &BTreeSet::from([r.clone()]),
                        &catalogs,
                        &deployments,
                        &allowed,
                    )
                    .unwrap();
                assert_eq!(
                    card["declaration"],
                    serde_json::to_value(&canonical.capabilities[&r].declaration).unwrap()
                );
                found = true;
            }
        }
        assert!(found, "full target contract absent");
        for id in local["definition_refs"].as_array().unwrap() {
            assert!(body["state"]["definitions"]
                .get(id.as_str().unwrap())
                .is_some());
        }
        for id in local["binding_refs"].as_array().unwrap() {
            assert!(body["state"]["binding_witnesses"]
                .get(id.as_str().unwrap())
                .is_some());
        }
    }
    let primary = request
        .bindings
        .iter()
        .find(|(_, id)| **id == index.candidates[&reference("op_a")].id)
        .expect("primary target must be issued")
        .0
        .clone();
    (request, primary)
}

#[tokio::test]
#[ignore = "explicit frozen 48-packet diagnostic runner only"]
async fn frozen_decision_interface_v1() {
    let root = PathBuf::from(crate::test_manifest_dir()).join("../../fixtures");
    let cases: Vec<Case> = serde_json::from_slice(
        &std::fs::read(root.join("discovery/decision-interface-v1/cases.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cases.len(), 48);
    let out = PathBuf::from(std::env::var("PLASM_DECISION_DIAGNOSTIC_OUT").unwrap());
    let mode = std::env::var("PLASM_DECISION_DIAGNOSTIC_MODE").unwrap();
    assert!(matches!(mode.as_str(), "export" | "live"));
    // Validate every scope before the first possible provider call.
    let issued: Vec<_> = cases.iter().map(|case| prepared(case, &root)).collect();
    let packets: Vec<_> = cases.iter().zip(&issued).map(|(case, (request, primary))| {
        json!({"id":case.id,"pair":case.pair,"family":case.family,"body":request.body(),
               "primary_question":primary,"bytes":request.body().len(),"sha256":content_hash(request.body().as_bytes())})
    }).collect();
    if mode == "export" {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out.join("PACKETS.json"))
            .unwrap();
        serde_json::to_writer_pretty(&mut file, &packets).unwrap();
        return;
    }
    let frozen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("PACKETS.json")).unwrap()).unwrap();
    assert_eq!(frozen, json!(packets), "frozen packet drift");
    let credential = std::fs::read_to_string(out.join("credential.private")).unwrap();
    let transport = plasm_runtime::provider_transport::ReqwestProviderTransport::new(
        reqwest::Client::new(),
        RELEVANCE_ENDPOINT,
        credential.trim().to_owned(),
    );
    let mut trace = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("TRACE.jsonl"))
        .unwrap();
    for ((case, (request, _)), frozen) in cases.iter().zip(&issued).zip(&packets) {
        writeln!(trace, "{}", json!({"kind":"logical_start","id":case.id,"sha256":frozen["sha256"],"bytes":request.body().len()})).unwrap();
        trace.flush().unwrap();
        let started = std::time::Instant::now();
        let raw = plasm_runtime::provider_transport::request_provider(
            &transport,
            request.body(),
            Default::default(),
            |record| {
                writeln!(
                    trace,
                    "{}",
                    json!({"kind":"transport","id":case.id,"record":record})
                )?;
                trace.flush()?;
                Ok(())
            },
        )
        .await;
        let result = match raw {
            Ok(raw) => match decode_batch(request, &raw) {
                Ok(_) => Ok(raw),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        match result {
            Ok(raw) => writeln!(trace, "{}", json!({"kind":"logical_complete","id":case.id,"elapsed_ms":started.elapsed().as_millis(),"raw":raw})).unwrap(),
            Err(error) => {
                writeln!(trace, "{}", json!({"kind":"fault","id":case.id,"error":error})).unwrap();
                panic!("unrecovered diagnostic fault; no quality retries");
            }
        }
        trace.flush().unwrap();
    }
    writeln!(
        trace,
        "{}",
        json!({"kind":"complete","logical_requests":48})
    )
    .unwrap();
}
