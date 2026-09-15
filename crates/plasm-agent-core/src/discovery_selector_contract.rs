//! Model-only wire contract. Host receipts are produced only after strict validation.
use super::{CapabilitySelection, RequirementCoverage, RetrievalReceipt};
use anyhow::{bail, Context, Result};
use plasm_core::catalog_discovery::content_hash;
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

/// OpenRouter decoding pin. Temperature 0 alone is not a stable card.
pub(crate) const SELECTOR_SEED: i64 = 42;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    additional_capability_ids: Vec<String>,
    requirement_coverage: Vec<RequirementCoverage>,
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
    let ids: BTreeSet<_> = retrieval.candidates.iter().map(|c| &c.id).collect();
    if ids.len() != retrieval.candidates.len() || ids.iter().any(|id| id.trim().is_empty()) {
        bail!("selector evidence has empty or duplicate candidate IDs");
    }
    let mut already_exposed: Vec<_> = exposed.to_vec();
    already_exposed.sort();
    let mut candidates = retrieval.candidates.clone();
    candidates.sort_by(|a, b| a.reference.cmp(&b.reference).then_with(|| a.id.cmp(&b.id)));
    let request = json!({
        "model": model, "temperature": 0, "seed": SELECTOR_SEED,
        "provider": {"require_parameters": true},
        "messages": [
            {"role":"system", "content": INSTRUCTIONS},
            {"role":"user", "content": serde_json::to_string(&json!({"intent":intent,"already_exposed":already_exposed,"candidates":candidates}))?}
        ],
        "response_format": {"type":"json_schema","json_schema":{"name":"capability_selection","strict":true,"schema":schema(retrieval)}}
    });
    serde_json::to_string(&request).context("serialize capability selector request")
}

/// SHA-256 of the exact OpenRouter body (model, seed, instructions, intent, exposed, candidates).
/// Changing `INSTRUCTIONS` or the response schema changes this key automatically.
pub fn request_cache_key(request_body: &str) -> String {
    content_hash(request_body.as_bytes())
}

pub fn selection_envelope(selection: &CapabilitySelection) -> Result<String> {
    serde_json::to_string(&json!({
        "additional_capability_ids": selection.additional_capability_ids,
        "requirement_coverage": selection.requirement_coverage,
    }))
    .context("serialize cached capability selection")
}

pub fn wrap_cached_envelope(envelope: &str) -> String {
    json!({"choices":[{"finish_reason":"stop","message":{"content": envelope}}]}).to_string()
}

/// Decode and strictly validate a provider envelope. Performs no acquisition or repair.
pub fn decode(
    raw: &str,
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
        envelope.requirement_coverage,
        retrieval,
    )?;
    let business = super::validate_selection(&selection, retrieval)?;
    Ok((selection, business))
}

pub(crate) fn save_rejection(directory: &Path, record: &Value) -> Result<PathBuf> {
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

/// Selector system instructions.
///
/// Coverage is explicit per requirement (supporting IDs or unresolved reason).
/// Do not reintroduce task-shaped special cases (app brands, confirmation recipes).
pub(crate) const INSTRUCTIONS: &str = include_str!("discovery_selector_prompt.txt");

/// Offered `revision/capability` IDs, sorted. This is the only legal decode universe.
pub(crate) fn offered_ids(retrieval: &RetrievalReceipt) -> Vec<String> {
    retrieval
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn schema(retrieval: &RetrievalReceipt) -> Value {
    let offered = offered_ids(retrieval);
    json!({"type":"object","additionalProperties":false,
    "required":["additional_capability_ids","requirement_coverage"],
    "properties":{
        "additional_capability_ids":{"type":"array","items":{"type":"string","enum":offered.clone()}},
        "requirement_coverage":{"type":"array","items":{"type":"object","additionalProperties":false,
            "required":["requirement","supporting_capability_ids","unresolved_reason"],
            "properties":{
                "requirement":{"type":"string","minLength":1},
                "supporting_capability_ids":{"type":"array","items":{"type":"string","enum":offered}},
                "unresolved_reason":{"type":"string"}
            }}}
    }})
}
