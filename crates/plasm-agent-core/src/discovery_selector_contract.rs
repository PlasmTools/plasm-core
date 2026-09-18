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
    let candidates = wire_candidates(retrieval);
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

pub fn selection_envelope(
    selection: &CapabilitySelection,
    retrieval: &RetrievalReceipt,
) -> Result<String> {
    super::validate_selection(selection, retrieval)?;
    let aliases: std::collections::BTreeMap<_, _> = sorted_candidates(retrieval)
        .into_iter()
        .enumerate()
        .map(|(n, c)| (c.id.as_str(), format!("c{n}")))
        .collect();
    let mut coverage = selection.requirement_coverage.clone();
    for requirement in &mut coverage {
        for id in requirement.assessment.capability_ids_mut() {
            *id = aliases
                .get(id.as_str())
                .context("cached selection references unknown candidate")?
                .clone();
        }
    }
    serde_json::to_string(&json!({"requirement_coverage": coverage}))
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
    let aliases = candidate_aliases(retrieval);
    let mut coverage = envelope.requirement_coverage;
    for requirement in &mut coverage {
        for id in requirement.assessment.capability_ids_mut() {
            *id = aliases
                .get(id.as_str())
                .context("requirement coverage references an unknown candidate ID")?
                .clone();
        }
    }
    let selection = CapabilitySelection::from_coverage(coverage, retrieval)?;
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
/// Coverage is explicit per requirement (support or a gap with useful IDs).
/// Do not reintroduce task-shaped special cases (app brands, confirmation recipes).
pub(crate) const INSTRUCTIONS: &str = include_str!("discovery_selector_prompt.txt");

/// Stable request-local aliases. Canonical revision IDs never cross the model wire.
fn sorted_candidates(
    retrieval: &RetrievalReceipt,
) -> Vec<&crate::discovery_store::RetrievedCapability> {
    let mut candidates: Vec<_> = retrieval.candidates.iter().collect();
    candidates.sort_by(|a, b| a.reference.cmp(&b.reference).then_with(|| a.id.cmp(&b.id)));
    candidates
}

/// The same canonical ordering owns both wire emission and host resolution.
pub(crate) fn candidate_aliases(
    retrieval: &RetrievalReceipt,
) -> std::collections::BTreeMap<String, String> {
    sorted_candidates(retrieval)
        .into_iter()
        .enumerate()
        .map(|(n, c)| (format!("c{n}"), c.id.clone()))
        .collect()
}

pub(crate) fn wire_candidates(
    retrieval: &RetrievalReceipt,
) -> Vec<crate::discovery_store::RetrievedCapability> {
    sorted_candidates(retrieval)
        .into_iter()
        .enumerate()
        .map(|(n, c)| {
            let mut candidate = c.clone();
            candidate.id = format!("c{n}");
            candidate
        })
        .collect()
}

pub(crate) fn offered_ids(retrieval: &RetrievalReceipt) -> Vec<String> {
    (0..retrieval.candidates.len())
        .map(|n| format!("c{n}"))
        .collect()
}

pub(crate) fn schema(retrieval: &RetrievalReceipt) -> Value {
    let offered = offered_ids(retrieval);
    let evidence = if offered.is_empty() {
        json!({"type":"array","maxItems":0,"items":{"type":"string"}})
    } else {
        json!({"type":"array","items":{"type":"string","enum":offered}})
    };
    let mut assessments = Vec::new();
    if !offered.is_empty() {
        let mut supported = evidence.clone();
        supported["minItems"] = json!(1);
        assessments.push(
            json!({"type":"object","additionalProperties":false,"required":["supported_by"],
            "properties":{"supported_by":supported}}),
        );
    }
    assessments.push(json!({"type":"object","additionalProperties":false,"required":["useful_capabilities","missing"],
        "properties":{
            "useful_capabilities":evidence,
            "missing":{"type":"string","minLength":1,"description":"The specific required capability or input that the presented documents do not support. Not an explanation of a valid composition."}
        }}));
    json!({"type":"object","additionalProperties":false,
    "required":["requirement_coverage"],
    "properties":{
        "requirement_coverage":{"type":"array","items":{"type":"object","additionalProperties":false,
            "required":["requirement","assessment"],
            "properties":{
                "requirement":{"type":"string","minLength":1},
                "assessment":{"anyOf":assessments}
            }}}
    }})
}
