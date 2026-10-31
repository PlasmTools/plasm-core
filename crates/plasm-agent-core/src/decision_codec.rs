//! Provider serialization seam. JSON exists only at encode/decode, never as
//! selection state. Each decoder receives the binding context issued with its request.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderResponseError {
    #[error("malformed decision response envelope")]
    MalformedEnvelope(#[source] serde_json::Error),
    #[error("decision response resolved to an unexpected model")]
    UnexpectedModel,
    #[error("decision response came from an unexpected provider")]
    UnexpectedProvider,
    #[error("decision response has missing, extra or duplicate answers")]
    AnswerSetMismatch,
    #[error("decision answer has an unexpected question type")]
    UnexpectedQuestionType,
    #[error("decision answer contains an unsupported choice")]
    UnsupportedChoice,
    #[error("decision answer probability keys differ from the requested choices")]
    ProbabilityKeysMismatch,
    #[error("decision answer probability vector is invalid")]
    InvalidProbabilities,
    #[error("decision answer confidence is invalid")]
    InvalidConfidence,
}

#[derive(Debug, Error)]
pub enum DecisionCodecError {
    #[error("decision request serialization failed")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    Response(#[from] ProviderResponseError),
}

const MAX_ROUNDED_PROBABILITY_DRIFT: f64 = 0.011;

#[derive(Deserialize)]
struct ChoiceEnvelope {
    model: String,
    provider: String,
    #[serde(deserialize_with = "unique_map")]
    answers: BTreeMap<String, ChoiceAnswer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChoiceAnswer {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    confidence: f64,
    #[serde(deserialize_with = "unique_map")]
    probabilities: BTreeMap<String, f64>,
}

/// Validated provider choice. Auxiliary scores are validated, but only the choice
/// participates in contribution classification; raw provider evidence is retained.
pub(crate) struct ValidatedChoice {
    pub choice: String,
}

/// Match an issued model alias or its eight-digit dated revision.
pub(crate) fn model_matches(issued: &str, resolved: &str) -> bool {
    let is_date = |date: &str| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit());
    resolved == issued
        || (!issued
            .rsplit_once('-')
            .is_some_and(|(_, suffix)| is_date(suffix))
            && resolved
                .strip_prefix(issued)
                .is_some_and(|suffix| suffix.strip_prefix('-').is_some_and(is_date)))
}

/// Decode Luna Decisions against the issued question and choice sets.
/// Provider envelope metadata is allowed; answer fields and binding maps are strict.
pub(crate) fn decode_choices<'a>(
    model: &str,
    questions: impl IntoIterator<Item = &'a str>,
    choices: &[&str],
    raw: &str,
) -> Result<BTreeMap<String, ValidatedChoice>, ProviderResponseError> {
    let envelope: ChoiceEnvelope =
        serde_json::from_str(raw).map_err(ProviderResponseError::MalformedEnvelope)?;
    if !model_matches(model, &envelope.model) {
        return Err(ProviderResponseError::UnexpectedModel);
    }
    if envelope.provider != "OpenAI" {
        return Err(ProviderResponseError::UnexpectedProvider);
    }
    let questions: BTreeSet<_> = questions.into_iter().collect();
    if envelope.answers.len() != questions.len()
        || !envelope
            .answers
            .keys()
            .all(|key| questions.contains(key.as_str()))
    {
        return Err(ProviderResponseError::AnswerSetMismatch);
    }
    envelope
        .answers
        .into_iter()
        .map(|(question, answer)| {
            if answer.kind != "choice" {
                return Err(ProviderResponseError::UnexpectedQuestionType);
            }
            if !choices.contains(&answer.choice.as_str()) {
                return Err(ProviderResponseError::UnsupportedChoice);
            }
            if answer.probabilities.len() != choices.len()
                || !choices
                    .iter()
                    .all(|key| answer.probabilities.contains_key(*key))
            {
                return Err(ProviderResponseError::ProbabilityKeysMismatch);
            }
            let total: f64 = answer.probabilities.values().sum();
            if !answer
                .probabilities
                .values()
                .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                || total <= 0.0
                || (total - 1.0).abs() > MAX_ROUNDED_PROBABILITY_DRIFT
            {
                return Err(ProviderResponseError::InvalidProbabilities);
            }
            if !answer.confidence.is_finite() || !(0.0..=1.0).contains(&answer.confidence) {
                return Err(ProviderResponseError::InvalidConfidence);
            }
            Ok((
                question,
                ValidatedChoice {
                    choice: answer.choice,
                },
            ))
        })
        .collect()
}

pub(crate) trait DecisionCodec {
    type Request<'a>: Serialize;

    fn encode(request: &Self::Request<'_>) -> std::result::Result<String, DecisionCodecError> {
        // Canonical map order makes identities independent of Rust field declaration order.
        Ok(serde_json::to_string(&serde_json::to_value(request)?)?)
    }
}

/// serde's ordinary map decoder overwrites duplicate keys. Provider decisions
/// must reject those ambiguous bindings instead of accepting the last answer.
pub(crate) fn unique_map<'de, D, V>(
    deserializer: D,
) -> std::result::Result<std::collections::BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    struct Visitor<V>(std::marker::PhantomData<V>);
    impl<'de, V: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<V> {
        type Value = std::collections::BTreeMap<String, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique decision keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = std::collections::BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, V>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate decision key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope() -> serde_json::Value {
        json!({
            "model": "openai/gpt-6-luna-decisions-20261006",
            "provider": "OpenAI",
            "answers": {"q": {
                "type": "choice", "choice": "yes", "confidence": 0.8,
                "probabilities": {"yes": 0.66, "no": 0.33}
            }}
        })
    }

    fn decode(
        raw: &str,
    ) -> std::result::Result<BTreeMap<String, ValidatedChoice>, ProviderResponseError> {
        decode_choices("openai/gpt-6-luna-decisions", ["q"], &["yes", "no"], raw)
    }

    #[test]
    fn accepts_model_revision_and_bounded_rounding_drift() {
        let mut input = envelope();
        input["usage"] = json!({"total_tokens": 20});
        let output = decode(&input.to_string()).unwrap();
        let answer = &output["q"];
        assert_eq!(answer.choice, "yes");
    }

    #[test]
    fn pinned_revision_requires_exact_resolution() {
        let alias = "openai/gpt-6-luna-decisions";
        let pinned = "openai/gpt-6-luna-decisions-20261006";
        assert!(model_matches(alias, pinned));
        assert!(model_matches(pinned, pinned));
        assert!(!model_matches(
            pinned,
            "openai/gpt-6-luna-decisions-20261007"
        ));
        assert!(!model_matches(
            pinned,
            "openai/gpt-6-luna-decisions-20261006-20261007"
        ));
    }

    #[test]
    fn rejects_provider_binding_choice_and_probability_corruption() {
        for (pointer, value) in [
            ("/provider", json!("Other")),
            ("/model", json!("openai/gpt-6-luna-decisions2")),
            ("/model", json!("openai/gpt-6-luna-decisions-other")),
            ("/provider", json!("TypeSafe")),
            ("/answers", json!({})),
            ("/answers/q/type", json!("score")),
            ("/answers/q/choice", json!("maybe")),
            ("/answers/q/confidence", json!(-0.1)),
            (
                "/answers/q/probabilities",
                json!({"yes": 0.5, "other": 0.5}),
            ),
            ("/answers/q/probabilities", json!({"yes": -0.1, "no": 1.1})),
            ("/answers/q/probabilities", json!({"yes": 0.5, "no": 0.1})),
            ("/answers/q/probabilities", json!({"yes": 0.0, "no": 0.0})),
        ] {
            let mut input = envelope();
            *input.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&input.to_string()).is_err(), "accepted {pointer}");
        }
        let mut input = envelope();
        input["answers"]["other"] = input["answers"]["q"].clone();
        assert!(decode(&input.to_string()).is_err());
        let mut input = envelope();
        input["answers"]["q"]["extra"] = json!(true);
        assert!(decode(&input.to_string()).is_err());
    }

    #[test]
    fn rejects_duplicate_answer_probability_and_struct_keys() {
        let answer = envelope()["answers"]["q"].to_string();
        let duplicate_answer = format!(
            r#"{{"model":"openai/gpt-6-luna-decisions","provider":"OpenAI","answers":{{"q":{answer},"q":{answer}}}}}"#
        );
        assert!(decode(&duplicate_answer).is_err());
        let raw = envelope().to_string();
        for duplicate in [
            raw.replace("\"yes\":0.66", "\"yes\":0.66,\"yes\":0.66"),
            raw.replace(
                "\"confidence\":0.8",
                "\"confidence\":0.8,\"confidence\":0.8",
            ),
            raw.replace(
                "\"provider\":\"OpenAI\"",
                "\"provider\":\"OpenAI\",\"provider\":\"OpenAI\"",
            ),
        ] {
            assert_ne!(raw, duplicate);
            assert!(decode(&duplicate).is_err());
        }
    }
}
