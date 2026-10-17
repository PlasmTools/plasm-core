//! Provider-free wire contract: serialize the same receipt type used by NAPI.
use plasm_agent_core::discovery_matcher::MatchChoice;
use plasm_agent_core::discovery_service::RoutingReceipt;
use plasm_agent_core::discovery_support::{EnvironmentSupport, SupportChoice};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/discovery/partial-routing.json"
    ))?;
    let mut receipt: RoutingReceipt = serde_json::from_value(packet["routing"].take())?;
    let mut cases = Vec::new();
    for choice in [
        SupportChoice::Established,
        SupportChoice::NotEstablished,
        SupportChoice::Undetermined,
    ] {
        let support = EnvironmentSupport {
            choice,
            request_hash: "abstract-request".into(),
        };
        let guidance = support.guidance();
        receipt.environment_support = Some(support);
        packet["routing"] = serde_json::to_value(&receipt)?;
        cases.push(serde_json::json!({"packet": packet, "guidance": guidance}));
    }
    receipt.environment_support = None;
    packet["routing"] = serde_json::to_value(&receipt)?;
    cases.push(serde_json::json!({"packet": packet, "guidance": null}));
    // Exercise the producer's actual selection policy before serializing for TS.
    for mixed in [false, true] {
        let mut retained: RoutingReceipt = serde_json::from_value(serde_json::to_value(&receipt)?)?;
        if mixed {
            let mut candidate = retained.retrieval.candidates[0].clone();
            candidate.id = "matrix/record_inspect".into();
            candidate.reference.capability = "record_inspect".into();
            retained.retrieval.candidates.push(candidate);
            let mut decision = retained.matching.matches[0].clone();
            decision.capability_id = "matrix/record_inspect".into();
            retained.matching.matches.push(decision);
        }
        let decision = retained.matching.matches.last_mut().unwrap();
        decision.choice = MatchChoice::Uncertain;
        decision.probabilities = [
            ("relevant".into(), 0.0),
            ("unrelated".into(), 0.0),
            ("uncertain".into(), 1.0),
        ]
        .into();
        decision.confidence = 1.0;
        retained.closure.as_mut().unwrap().business =
            retained.matching.selected(&retained.retrieval);
        packet["routing"] = serde_json::to_value(retained)?;
        cases.push(serde_json::json!({"packet": packet, "guidance": null}));
    }
    println!("{}", serde_json::to_string(&cases)?);
    Ok(())
}
