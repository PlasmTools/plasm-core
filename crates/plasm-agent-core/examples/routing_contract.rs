//! Provider-free wire contract: serialize the same receipt type used by NAPI.
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
    println!("{}", serde_json::to_string(&cases)?);
    Ok(())
}
