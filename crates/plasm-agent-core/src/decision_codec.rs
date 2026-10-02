//! Provider serialization seam. JSON exists only at encode/decode, never as
//! selection state. Each decoder receives the binding context issued with its request.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

/// Validated provider choice; probabilities are normalized after accepting only
/// bounded provider rounding drift. Interpretation belongs to the caller.
pub(crate) struct ValidatedChoice {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

/// Decode the shared Jev protocol against the issued question and choice sets.
/// Provider envelope metadata is allowed; answer fields and binding maps are strict.
pub(crate) fn decode_jev_choices<'a>(
    model: &str,
    questions: impl IntoIterator<Item = &'a str>,
    choices: &[&str],
    raw: &str,
) -> Result<BTreeMap<String, ValidatedChoice>> {
    let envelope: ChoiceEnvelope =
        serde_json::from_str(raw).context("malformed Jev Decisions envelope")?;
    ensure!(
        envelope.model == model
            || envelope
                .model
                .strip_prefix(model)
                .is_some_and(|suffix| suffix.starts_with('-')),
        "unexpected Jev resolved model"
    );
    ensure!(envelope.provider == "TypeSafe", "unexpected Jev provider");
    let questions: BTreeSet<_> = questions.into_iter().collect();
    ensure!(
        envelope.answers.len() == questions.len()
            && envelope
                .answers
                .keys()
                .all(|key| questions.contains(key.as_str())),
        "Jev response has missing or extra answers"
    );
    envelope
        .answers
        .into_iter()
        .map(|(question, answer)| {
            ensure!(answer.kind == "choice", "Jev answer is not a choice");
            ensure!(
                choices.contains(&answer.choice.as_str()),
                "Jev answer has unknown choice"
            );
            ensure!(
                answer.probabilities.len() == choices.len()
                    && choices
                        .iter()
                        .all(|key| answer.probabilities.contains_key(*key)),
                "Jev answer probability keys differ from requested choices"
            );
            let total: f64 = answer.probabilities.values().sum();
            ensure!(
                answer
                    .probabilities
                    .values()
                    .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                    && total > 0.0
                    && (total - 1.0).abs() <= MAX_ROUNDED_PROBABILITY_DRIFT,
                "invalid Jev probability vector"
            );
            ensure!(
                answer.confidence.is_finite() && (0.0..=1.0).contains(&answer.confidence),
                "invalid Jev confidence"
            );
            Ok((
                question,
                ValidatedChoice {
                    choice: answer.choice,
                    confidence: answer.confidence,
                    probabilities: answer
                        .probabilities
                        .into_iter()
                        .map(|(key, probability)| (key, probability / total))
                        .collect(),
                },
            ))
        })
        .collect()
}

pub(crate) trait DecisionCodec {
    type Request<'a>: Serialize;
    type Context: ?Sized;
    type Output;

    fn encode(request: &Self::Request<'_>) -> Result<String> {
        // Canonical map order makes identities independent of Rust field declaration order.
        Ok(serde_json::to_string(&serde_json::to_value(request)?)?)
    }
    fn decode(context: &Self::Context, raw: &str) -> Result<Self::Output>;
}

/// serde's ordinary map decoder overwrites duplicate keys. Provider decisions
/// must reject those ambiguous bindings instead of accepting the last answer.
pub(crate) fn unique_map<'de, D, V>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, V>, D::Error>
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
        ) -> Result<Self::Value, A::Error> {
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
            "model": "typesafe/jev-test-20261002",
            "provider": "TypeSafe",
            "answers": {"q": {
                "type": "choice", "choice": "yes", "confidence": 0.8,
                "probabilities": {"yes": 0.66, "no": 0.33}
            }}
        })
    }

    fn decode(raw: &str) -> Result<BTreeMap<String, ValidatedChoice>> {
        decode_jev_choices("typesafe/jev-test", ["q"], &["yes", "no"], raw)
    }

    #[test]
    fn accepts_model_revision_and_normalizes_only_rounding_drift() {
        let mut input = envelope();
        input["usage"] = json!({"total_tokens": 20});
        let output = decode(&input.to_string()).unwrap();
        let answer = &output["q"];
        assert_eq!(answer.choice, "yes");
        assert_eq!(answer.confidence, 0.8);
        assert!((answer.probabilities.values().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((answer.probabilities["yes"] - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn rejects_provider_binding_choice_and_probability_corruption() {
        for (pointer, value) in [
            ("/provider", json!("Other")),
            ("/model", json!("typesafe/jev-test2")),
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
            r#"{{"model":"typesafe/jev-test","provider":"TypeSafe","answers":{{"q":{answer},"q":{answer}}}}}"#
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
                "\"provider\":\"TypeSafe\"",
                "\"provider\":\"TypeSafe\",\"provider\":\"TypeSafe\"",
            ),
        ] {
            assert_ne!(raw, duplicate);
            assert!(decode(&duplicate).is_err());
        }
    }
}
