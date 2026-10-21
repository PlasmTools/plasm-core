//! Strict chat-completion boundary for relevance choices. No synthetic confidence.
use super::MatchChoice;
use crate::decision_codec::DecisionResponseError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub model: &'a str,
    pub messages: [Message; 4],
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
    rules: String,
    intent: String,
    questions: String,
) -> Request<'a> {
    Request {
        model,
        messages: [
            Message { role: "system", content: "Answer each supplied choice question using the shared rules and intent together with that question's complete evidence. Return one answer for each supplied question key, exactly once, and no other keys. Return only JSON matching the supplied output schema.".into() },
            Message { role: "user", content: rules },
            Message { role: "user", content: intent },
            Message { role: "user", content: questions },
        ],
        reasoning: Reasoning { effort: "high" },
        max_tokens: 16_384,
        response_format: serde_json::json!({"type":"json_schema", "json_schema": {
            "name":"discovery_relevance", "strict":true,
            "schema": {"type":"object", "additionalProperties":false, "required":["answers"],
                "properties":{"answers":{"type":"array", "items":{
                    "type":"object", "additionalProperties":false, "required":["key","choice"],
                    "properties":{"key":{"type":"string"},
                        "choice":{"type":"string","enum":["relevant","unrelated","uncertain"]}}
                }}}}
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
    answers: Vec<Answer>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    key: String,
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
    let expected: BTreeSet<_> = keys.collect();
    let mut admitted = BTreeMap::new();
    for answer in answers.answers {
        if !expected.contains(&answer.key) || admitted.insert(answer.key, answer.choice).is_some() {
            return Err(DecisionResponseError::AnswerSetMismatch);
        }
    }
    if admitted.len() != expected.len() {
        return Err(DecisionResponseError::AnswerSetMismatch);
    }
    Ok(admitted)
}
