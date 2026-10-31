//! Native Decisions relevance boundary: shared evidence, independently bound questions.
use super::{MatchChoice, SharedEvidence};
use crate::decision_codec::{decode_choices, ProviderResponseError};
use serde::Serialize;
use std::collections::BTreeMap;

pub(crate) const ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub model: &'a str,
    pub state: &'a SharedEvidence<'a>,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Serialize)]
pub(super) struct Question {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub instructions: String,
    pub criteria: BTreeMap<&'static str, &'static str>,
}

pub(super) fn decode<'a>(
    model: &str,
    keys: impl Iterator<Item = &'a String>,
    raw: &str,
) -> Result<BTreeMap<String, MatchChoice>, ProviderResponseError> {
    decode_choices(
        model,
        keys.map(String::as_str),
        &["relevant", "uncertain", "unrelated"],
        raw,
    )?
    .into_iter()
    .map(|(key, answer)| {
        let choice = match answer.choice.as_str() {
            "relevant" => MatchChoice::Relevant,
            "uncertain" => MatchChoice::Uncertain,
            "unrelated" => MatchChoice::Unrelated,
            _ => return Err(ProviderResponseError::UnsupportedChoice),
        };
        Ok((key, choice))
    })
    .collect()
}
