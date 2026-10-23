//! Provider-free wire contract: serialize the same receipt type used by NAPI.
use plasm_agent_core::discovery_matcher::MatchChoice;
use plasm_agent_core::discovery_service::RoutingReceipt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/discovery/partial-routing.json"
    ))?;
    let receipt: RoutingReceipt = serde_json::from_value(packet["routing"].take())?;
    let mut cases = Vec::new();
    packet["routing"] = serde_json::to_value(&receipt)?;
    cases.push(serde_json::json!({"packet": packet}));
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
        retained.closure.as_mut().unwrap().business =
            retained.matching.selected(&retained.retrieval);
        packet["routing"] = serde_json::to_value(retained)?;
        cases.push(serde_json::json!({"packet": packet}));
    }
    println!("{}", serde_json::to_string(&cases)?);
    Ok(())
}
