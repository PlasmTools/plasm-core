//! Inspect the model-facing projection of a retained execution receipt.
use plasm_agent_core::workflow_learning::{model::ExecutionReceipt, review_packet::ReviewPacket};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("expected receipt path")?;
    let receipt: ExecutionReceipt = serde_json::from_slice(&std::fs::read(path)?)?;
    let packet = ReviewPacket::from_receipt(&receipt)
        .encode()?
        .ok_or("review packet exceeds budget")?;
    println!("{}", serde_json::to_string_pretty(&packet)?);
    Ok(())
}
