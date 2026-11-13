//! Native Decisions relevance boundary: shared evidence, independently bound questions.
use super::{MatchChoice, SharedEvidence};
use crate::decision_codec::{decode_choices, ProviderResponseError};
use serde::Serialize;
use std::collections::BTreeMap;

pub(crate) const ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub model: &'a str,
    pub state: State<'a>,
    pub questions: BTreeMap<String, Question>,
}

/// The same Decisions protocol carries either the existing graph witness or an
/// operation-bound witness; neither representation grants catalogue access.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum State<'a> {
    Graph(&'a SharedEvidence<'a>),
    #[cfg(test)]
    Operation(&'a super::operations::DecisionState<'a>),
    OperationGraph(&'a super::operations::GraphDecisionState<'a>),
}

#[derive(Serialize)]
#[cfg_attr(test, derive(Clone))]
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
