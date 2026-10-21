//! Strict chat-completion boundary for relevance choices. No synthetic confidence.
use super::MatchChoice;
use crate::decision_codec::{unique_map, DecisionResponseError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub model: &'a str,
    pub messages: [Message; 2],
    pub reasoning: Reasoning,
    pub max_tokens: u32,
    pub response_format: serde_json::Value,
}
#[derive(Serialize)]
pub(super) struct Message {
    pub role: &'static str,
    pub content: String,
}
#[derive(Serialize)]
pub(super) struct Reasoning {
    pub effort: &'static str,
}

pub(super) fn request<'a>(
    model: &'a str,
    content: String,
    keys: impl Iterator<Item = &'a String>,
) -> Request<'a> {
    let properties: BTreeMap<_, _> = keys.map(|key| (key.clone(), serde_json::json!({
        "type": "object", "additionalProperties": false, "required": ["choice"],
        "properties": {"choice": {"type": "string", "enum": ["relevant", "unrelated", "uncertain"]}}
    }))).collect();
    Request {
        model,
        messages: [
            Message { role: "system", content: "Answer each supplied choice question. Return only JSON matching the supplied output schema.".into() },
            Message { role: "user", content },
        ],
        reasoning: Reasoning { effort: "high" },
        max_tokens: 16_384,
        response_format: serde_json::json!({"type":"json_schema", "json_schema": {
            "name":"discovery_relevance", "strict":true,
            "schema": {"type":"object", "additionalProperties":false, "required":["answers"],
                "properties":{"answers":{"type":"object", "additionalProperties":false,
                    "required":properties.keys().collect::<Vec<_>>(), "properties":properties}}}
        }}),
    }
}

#[derive(Deserialize)]
struct Envelope {
    model: String,
    choices: Vec<Completion>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Completion {
    finish_reason: String,
    message: AnswerMessage,
}
#[derive(Deserialize)]
struct AnswerMessage {
    role: String,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<Vec<serde_json::Value>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answers {
    #[serde(deserialize_with = "unique_map")]
    answers: BTreeMap<String, Answer>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    choice: MatchChoice,
}

pub(super) fn decode<'a>(
    model: &str,
    keys: impl Iterator<Item = &'a String>,
    raw: &str,
) -> Result<BTreeMap<String, MatchChoice>, DecisionResponseError> {
    let envelope: Envelope =
        serde_json::from_str(raw).map_err(DecisionResponseError::MalformedEnvelope)?;
    if envelope.error.is_some() {
        return Err(DecisionResponseError::InvalidCompletion);
    }
    if !(envelope.model == model
        || envelope
            .model
            .strip_prefix(model)
            .is_some_and(|suffix| suffix.starts_with('-')))
    {
        return Err(DecisionResponseError::UnexpectedModel);
    }
    let [completion]: [Completion; 1] = envelope
        .choices
        .try_into()
        .map_err(|_| DecisionResponseError::InvalidCompletion)?;
    let message = completion.message;
    if completion.finish_reason != "stop"
        || message.role != "assistant"
        || message.refusal.is_some()
        || message.tool_calls.is_some_and(|calls| !calls.is_empty())
    {
        return Err(DecisionResponseError::InvalidCompletion);
    }
    let content = message
        .content
        .ok_or(DecisionResponseError::InvalidCompletion)?;
    let answers: Answers =
        serde_json::from_str(&content).map_err(DecisionResponseError::MalformedEnvelope)?;
    if answers.answers.keys().collect::<BTreeSet<_>>() != keys.collect::<BTreeSet<_>>() {
        return Err(DecisionResponseError::AnswerSetMismatch);
    }
    Ok(answers
        .answers
        .into_iter()
        .map(|(key, answer)| (key, answer.choice))
        .collect())
}
