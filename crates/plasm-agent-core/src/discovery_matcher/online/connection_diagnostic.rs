//! Diagnostic-only, mechanically derived local connection rendering.
use super::*;

fn inline_connections(request: &mut IssuedBatch) {
    let mut body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
    let witnesses = body["state"]["binding_witnesses"].clone();
    for local in body["state"]["question_evidence"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        let mut lines = Vec::new();
        for id in local["binding_refs"].as_array().unwrap() {
            let w = &witnesses[id.as_str().unwrap()];
            assert_eq!(w["kind"], "input_source");
            let source = w["from"]["capability"].as_str().unwrap();
            let consumer = w["to"]["capability"].as_str().unwrap();
            assert!(local["capabilities"].get(source).is_some());
            assert!(local["capabilities"].get(consumer).is_some());
            lines.push(format!(
                "{}.{} -> {}.{}{}; collect={}",
                source,
                w["from"]["field"].as_str().unwrap(),
                consumer,
                w["to"]["input"]["lane"].as_str().unwrap(),
                serde_json::to_string(&w["to"]["input"]["path"]).unwrap(),
                w["collect"]
            ));
        }
        if !lines.is_empty() {
            local["verified_connections"] = json!(lines);
        }
    }
    request.body = serde_json::to_string(&body).unwrap();
    request.cache_key = content_hash(request.body.as_bytes());
    assert!(request.body.len() <= search_budget::MAX_INPUT_BYTES);
}

#[tokio::test]
#[ignore = "explicit frozen connection renderer diagnostic only"]
async fn frozen_connection_interface_v2() {
    let root = PathBuf::from(crate::test_manifest_dir()).join("../../fixtures");
    let cases: Vec<Case> = serde_json::from_slice(
        &std::fs::read(root.join("discovery/connection-interface-v2/cases.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cases.len(), 24);
    let out = PathBuf::from(std::env::var("PLASM_DECISION_DIAGNOSTIC_OUT").unwrap());
    let mode = std::env::var("PLASM_DECISION_DIAGNOSTIC_MODE").unwrap();
    assert!(matches!(mode.as_str(), "export" | "live"));
    let mut issued = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let (baseline, primary) = prepared(case, &root);
        for candidate in if i % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let mut request = baseline.clone();
            if candidate {
                inline_connections(&mut request);
            }
            let arm = if candidate { "inline" } else { "baseline" };
            issued.push((
                format!("{}-{arm}", case.id),
                case,
                arm,
                request,
                primary.clone(),
            ));
        }
    }
    let packets: Vec<_> = issued.iter().map(|(id, case, arm, request, primary)| json!({
        "id":id,"pair":case.id,"family":case.family,"arm":arm,"body":request.body(),
        "primary_question":primary,"bytes":request.body.len(),"sha256":content_hash(request.body.as_bytes())
    })).collect();
    if mode == "export" {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out.join("PACKETS.json"))
            .unwrap();
        serde_json::to_writer_pretty(file, &packets).unwrap();
        return;
    }
    let frozen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("PACKETS.json")).unwrap()).unwrap();
    assert_eq!(frozen, json!(packets));
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
    for ((id, _, _, request, _), packet) in issued.iter().zip(&packets) {
        writeln!(trace,"{}",json!({"kind":"logical_start","id":id,"sha256":packet["sha256"],"bytes":request.body.len()})).unwrap();
        trace.flush().unwrap();
        let start = std::time::Instant::now();
        let result = plasm_runtime::provider_transport::request_provider(
            &transport,
            request.body(),
            Default::default(),
            |record| {
                writeln!(
                    trace,
                    "{}",
                    json!({"kind":"transport","id":id,"record":record})
                )?;
                trace.flush()?;
                Ok(())
            },
        )
        .await
        .map_err(|e| e.to_string())
        .and_then(|raw| {
            decode_batch(request, &raw)
                .map(|_| raw)
                .map_err(|e| e.to_string())
        });
        match result {
            Ok(raw) => writeln!(trace,"{}",json!({"kind":"logical_complete","id":id,"elapsed_ms":start.elapsed().as_millis(),"raw":raw})).unwrap(),
            Err(error) => { writeln!(trace,"{}",json!({"kind":"fault","id":id,"error":error})).unwrap(); trace.flush().unwrap(); panic!("diagnostic aborted; no quality retry"); }
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
