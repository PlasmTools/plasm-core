//! Model-only wire contract. Host receipts are produced only after strict validation.
use super::{CapabilitySelection, RetrievalReceipt, UnsupportedWork};
use anyhow::{bail, Context, Result};
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    additional_capability_ids: Vec<String>,
    unsupported: Vec<UnsupportedWork>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}
#[derive(Deserialize)]
struct ChatChoice {
    finish_reason: String,
    message: ChatMessage,
}
#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

/// Build the production selector request without credentials or evaluation labels.
/// Candidates must already be scoped to the caller's authorization and generation.
pub fn request(
    model: &str,
    intent: &str,
    exposed: &[CapabilityRef],
    retrieval: &RetrievalReceipt,
) -> Result<String> {
    if model.trim().is_empty() || intent.trim().is_empty() {
        bail!("selector model and intent are required");
    }
    let ids: std::collections::BTreeSet<_> = retrieval.candidates.iter().map(|c| &c.id).collect();
    if ids.len() != retrieval.candidates.len() || ids.iter().any(|id| id.trim().is_empty()) {
        bail!("selector evidence has empty or duplicate candidate IDs");
    }
    let request = json!({
        "model": model, "temperature": 0, "provider": {"require_parameters": true},
        "messages": [
            {"role":"system", "content": INSTRUCTIONS},
            {"role":"user", "content": serde_json::to_string(&json!({"intent":intent,"already_exposed":exposed,"candidates":retrieval.candidates}))?}
        ],
        "response_format": {"type":"json_schema","json_schema":{"name":"capability_selection","strict":true,"schema":schema()}}
    });
    serde_json::to_string(&request).context("serialize capability selector request")
}

/// Decode and strictly validate a provider envelope. Performs no acquisition or repair.
pub fn decode(
    raw: &str,
    intent: &str,
    retrieval: &RetrievalReceipt,
) -> Result<(CapabilitySelection, Vec<CapabilityRef>)> {
    let response: ChatResponse =
        serde_json::from_str(raw).context("malformed capability selector envelope")?;
    if response.choices.len() != 1 {
        bail!("capability selector must return one choice");
    }
    let choice = response
        .choices
        .into_iter()
        .next()
        .context("selector choice missing")?;
    if choice.finish_reason != "stop" {
        bail!(
            "capability selector did not complete: {}",
            choice.finish_reason
        );
    }
    let envelope: Envelope =
        serde_json::from_str(&choice.message.content.context("selector content missing")?)
            .context("malformed capability selection")?;
    let selection = CapabilitySelection::from_capabilities(
        envelope.additional_capability_ids,
        envelope.unsupported,
        intent,
        retrieval,
    )?;
    let business = super::validate_selection(&selection, intent, retrieval)?;
    Ok((selection, business))
}

pub(super) fn save_rejection(directory: &Path, record: &Value) -> Result<PathBuf> {
    std::fs::create_dir_all(directory).context("create selector diagnostic directory")?;
    let path = directory.join(format!("selector-rejection-{}.json", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).context("create selector diagnostic")?;
    file.write_all(&serde_json::to_vec_pretty(record)?)
        .context("write selector diagnostic")?;
    file.sync_all().context("flush selector diagnostic")?;
    Ok(path)
}

pub(super) const INSTRUCTIONS: &str = include_str!("discovery_selector_prompt.txt");

pub(super) fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,
    "required":["additional_capability_ids","unsupported"],
    "properties":{
        "additional_capability_ids":{"type":"array","items":{"type":"string"}},
        "unsupported":{"type":"array","items":{"type":"object","additionalProperties":false,
            "required":["intent_quote","reason"],"properties":{
                "intent_quote":{"type":"string"},"reason":{"type":"string"}
            }}}
    }})
}
