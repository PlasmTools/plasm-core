//! Environment-level support is independent of capability relevance and execution.
use plasm_core::{
    catalog_discovery::{capability_documents, CapabilityDocument},
    prerequisites::CapabilityRef,
    CGS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

type Result<T> = std::result::Result<T, DiscoverySupportError>;

#[derive(Debug, Error)]
pub enum DiscoverySupportError {
    #[error("support judgment requires a model")]
    MissingModel,
    #[error("authorized support catalog is unavailable")]
    MissingCatalog,
    #[error("authorized support capability is unavailable")]
    MissingCapability,
    #[error("environment support request exceeds its byte bound")]
    RequestTooLarge,
    #[error("environment support response omits the issued answer")]
    MissingAnswer,
    #[error(transparent)]
    CatalogDiscovery(#[from] Box<plasm_core::catalog_discovery::CatalogDiscoveryError>),
    #[error(transparent)]
    DecisionResponse(#[from] crate::decision_codec::DecisionResponseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<plasm_core::catalog_discovery::CatalogDiscoveryError> for DiscoverySupportError {
    fn from(error: plasm_core::catalog_discovery::CatalogDiscoveryError) -> Self {
        Self::CatalogDiscovery(Box::new(error))
    }
}

const RULE: &str = "Judge the current intent jointly with the supplied available operation contracts. Earlier intent turns provide context; the last turn is current. Do the supplied contracts establish that these operations can fulfil the intent for the requested target, without assuming any unspecified identity association? Assess established support, not whether success might be possible. Missing identity evidence means support is not established; it does not prove execution impossible. Derive intermediate needs from this environment, not a fixed workflow. Do not infer identity from matching scalar types or neighboring fields. Judge capability-level support, not runtime data existence, execution permission or guaranteed success. This judgment does not remove relevant capabilities.";
const CHOICES: [&str; 3] = ["established", "not_established", "undetermined"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportChoice {
    Established,
    NotEstablished,
    Undetermined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSupport {
    pub choice: SupportChoice,
    pub request_hash: String,
}
impl EnvironmentSupport {
    pub fn guidance(&self) -> &'static str {
        match self.choice {
            SupportChoice::Established => "The supplied operation contracts establish capability-level support for the current intent. This is not a guarantee of matching data, successful execution or permission.",
            SupportChoice::NotEstablished => "The supplied operation contracts do not establish a plan for the current intent. Relevant capabilities remain available. Inspect their inputs and outputs, then extend the same logical_session_ref for unresolved needs. This does not mean the task is impossible or that a capability is absent from the catalog.",
            SupportChoice::Undetermined => "Support for the current intent remains undetermined from the supplied contracts. Relevant capabilities remain available. Inspect the evidence and extend the same logical_session_ref for unresolved needs; do not treat this as proof of impossibility.",
        }
    }
}

pub struct IssuedSupport {
    pub body: String,
    pub cache_key: String,
    model: String,
}
#[derive(Serialize)]
struct Card {
    reference: CapabilityRef,
    document: CapabilityDocument,
}
#[derive(Serialize)]
struct State<'a> {
    rule: &'static str,
    intent_turns: Vec<&'a str>,
    available_operations: Vec<Card>,
}
#[derive(Serialize)]
struct Question {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: &'static str,
    criteria: BTreeMap<&'static str, &'static str>,
}
#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    state: State<'a>,
    questions: BTreeMap<&'static str, Question>,
}

pub fn issue(
    model: &str,
    intent: &crate::intent_provenance::IntentProvenance,
    available: &BTreeSet<CapabilityRef>,
    catalogs: &BTreeMap<String, CGS>,
) -> Result<IssuedSupport> {
    if model.trim().is_empty() {
        return Err(DiscoverySupportError::MissingModel);
    }
    let mut cards = Vec::new();
    let catalog_ids: BTreeSet<_> = available.iter().map(|r| &r.catalog).collect();
    for catalog in catalog_ids {
        let cgs = catalogs
            .get(catalog)
            .ok_or(DiscoverySupportError::MissingCatalog)?;
        let mut documents: BTreeMap<_, _> = capability_documents(cgs)?
            .into_iter()
            .map(|d| (d.capability.clone(), d))
            .collect();
        for reference in available.iter().filter(|r| &r.catalog == catalog) {
            let document = documents
                .remove(reference.capability.as_str())
                .ok_or(DiscoverySupportError::MissingCapability)?;
            cards.push(Card {
                reference: reference.clone(),
                document,
            });
        }
    }
    let request=Request { model, state: State { rule:RULE, intent_turns:intent.turns().collect(), available_operations:cards }, questions:BTreeMap::from([("support",Question { kind:"choice", instructions:"Is support established by the supplied contracts, without assuming unspecified identity associations?", criteria:BTreeMap::from([
        ("established","The supplied contracts establish capability-level support."),
        ("not_established","The supplied contracts do not establish support; possibility is not enough."),
        ("undetermined","The supplied evidence does not allow this assessment."),
    ]) })]) };
    let body = serde_json::to_string(&request)?;
    // A whole-environment judgment cannot be truthfully replaced by independent pages.
    if body.len() > 128_000 {
        return Err(DiscoverySupportError::RequestTooLarge);
    }
    let cache_key = format!(
        "jev-environment-support-v1:{}",
        plasm_core::catalog_discovery::content_hash(body.as_bytes())
    );
    Ok(IssuedSupport {
        body,
        cache_key,
        model: model.into(),
    })
}

pub fn decode(issued: &IssuedSupport, raw: &str) -> Result<EnvironmentSupport> {
    let mut answers =
        crate::decision_codec::decode_jev_choices(&issued.model, ["support"], &CHOICES, raw)?;
    let answer = answers
        .remove("support")
        .ok_or(DiscoverySupportError::MissingAnswer)?;
    let choice = match answer.choice.as_str() {
        "established" => SupportChoice::Established,
        "not_established" => SupportChoice::NotEstablished,
        "undetermined" => SupportChoice::Undetermined,
        _ => unreachable!("shared codec validated the issued choices"),
    };
    Ok(EnvironmentSupport {
        choice,
        request_hash: issued.cache_key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn issued() -> IssuedSupport {
        issue(
            "typesafe/jev-1.13",
            &crate::intent_provenance::IntentProvenance::from_turns(["inspect records".to_owned()])
                .unwrap(),
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
        .unwrap()
    }
    #[test]
    fn support_packet_contains_only_available_fixture_capabilities() {
        let mut cgs = plasm_core::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.entry_id = Some("matrix".into());
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let intent =
            crate::intent_provenance::IntentProvenance::from_turns(["inspect records".into()])
                .unwrap();
        let selected = BTreeSet::from([CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        }]);
        let request = issue("typesafe/jev-1.13", &intent, &selected, &catalogs).unwrap();
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        let operations = body["state"]["available_operations"].as_array().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0]["reference"]["capability"], "read");
        assert_eq!(selected.len(), 1);
        let missing = BTreeSet::from([CapabilityRef {
            catalog: "other".into(),
            capability: "read".into(),
        }]);
        assert!(issue("typesafe/jev-1.13", &intent, &missing, &catalogs).is_err());
        assert_ne!(
            request.cache_key,
            issue("typesafe/jev-1.13", &intent, &BTreeSet::new(), &catalogs)
                .unwrap()
                .cache_key
        );
    }

    #[test]
    fn support_decoder_rejects_duplicate_and_extra_answers() {
        let raw = r#"{"model":"typesafe/jev-1.13","provider":"TypeSafe","answers":{"support":{"type":"choice","choice":"not_established","confidence":0.5,"probabilities":{"established":0.3,"not_established":0.4,"undetermined":0.3}},"support":{"type":"choice","choice":"established","confidence":0.5,"probabilities":{"established":0.3,"not_established":0.4,"undetermined":0.3}}}}"#;
        assert!(decode(&issued(), raw).is_err());
        assert!(decode(
            &issued(),
            r#"{"model":"typesafe/jev-1.13","provider":"TypeSafe","answers":{}}"#
        )
        .is_err());
    }

    #[test]
    fn support_contract_separates_established_from_possible() {
        let request = issued();
        assert!(request
            .body
            .contains("not whether success might be possible"));
        assert!(request
            .body
            .contains("does not remove relevant capabilities"));
        assert!(EnvironmentSupport {
            choice: SupportChoice::NotEstablished,
            request_hash: String::new()
        }
        .guidance()
        .contains("does not mean the task is impossible"));
    }
    #[test]
    fn support_decode_preserves_negative_and_uncertain() {
        for choice in ["established", "not_established", "undetermined"] {
            let raw = serde_json::json!({"provider":"TypeSafe","model":"typesafe/jev-1.13-20260917","answers":{"support":{"type":"choice","choice":choice,"confidence":0.5,"probabilities":{"established":0.3,"not_established":0.4,"undetermined":0.3}}}});
            assert!(decode(&issued(), &raw.to_string()).is_ok());
            let mut bad = raw.clone();
            bad["answers"]["support"]["probabilities"]["established"] = 2.into();
            assert!(decode(&issued(), &bad.to_string()).is_err());
            let mut bad = raw;
            bad["provider"] = "other".into();
            assert!(decode(&issued(), &bad.to_string()).is_err());
        }
    }
}
