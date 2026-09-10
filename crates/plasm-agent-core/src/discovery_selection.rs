//! Capability sufficiency validation; conversational decisions belong to the agent.
use super::RetrievalReceipt;
use anyhow::{bail, Context, Result};
use plasm_core::prerequisites::CapabilityRef;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStatus {
    Ready,
    Insufficient,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedWork {
    pub intent_quote: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySelection {
    /// Model-reported sufficiency of presented capabilities, never permission to execute.
    pub status: SelectionStatus,
    pub additional_capability_ids: Vec<String>,
    pub unsupported: Vec<UnsupportedWork>,
}

impl CapabilitySelection {
    pub fn from_capabilities(
        additional_capability_ids: Vec<String>,
        unsupported: Vec<UnsupportedWork>,
        intent: &str,
        retrieval: &RetrievalReceipt,
    ) -> Result<Self> {
        let selection = Self {
            status: if unsupported.is_empty() {
                SelectionStatus::Ready
            } else {
                SelectionStatus::Insufficient
            },
            additional_capability_ids,
            unsupported,
        };
        validate_selection(&selection, intent, retrieval)?;
        Ok(selection)
    }

    pub fn explanation_lines(&self) -> Vec<String> {
        self.unsupported
            .iter()
            .map(|work| format!("Unsupported: {} — {}", work.intent_quote, work.reason))
            .collect()
    }
}

pub(super) fn validate_selection(
    selection: &CapabilitySelection,
    intent: &str,
    retrieval: &RetrievalReceipt,
) -> Result<Vec<CapabilityRef>> {
    let candidates: BTreeMap<_, _> = retrieval
        .candidates
        .iter()
        .map(|c| (c.id.as_str(), &c.reference))
        .collect();
    if candidates.len() != retrieval.candidates.len()
        || candidates.keys().any(|id| id.trim().is_empty())
    {
        bail!("selector evidence has empty or duplicate candidate IDs");
    }
    let expected = if selection.unsupported.is_empty() {
        SelectionStatus::Ready
    } else {
        SelectionStatus::Insufficient
    };
    if selection.status != expected {
        bail!("selection status contradicts capability sufficiency");
    }
    if selection
        .additional_capability_ids
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        != selection.additional_capability_ids.len()
    {
        bail!("duplicate selected capability ID");
    }
    let selected: BTreeSet<_> = selection
        .additional_capability_ids
        .iter()
        .map(|id| {
            candidates
                .get(id.as_str())
                .map(|cap| (*cap).clone())
                .context("selector returned an unknown candidate ID")
        })
        .collect::<Result<_>>()?;
    let mut quotes = BTreeSet::new();
    for work in &selection.unsupported {
        if work.intent_quote.trim().is_empty()
            || work.reason.trim().is_empty()
            || !intent.contains(&work.intent_quote)
            || !quotes.insert(&work.intent_quote)
        {
            bail!("unsupported work requires a unique quote from the intent and a reason");
        }
    }
    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{discovery_service::selector_contract, discovery_store::RetrievedCapability};
    use plasm_core::catalog_discovery::CapabilityDocument;
    use serde_json::json;

    fn retrieval() -> RetrievalReceipt {
        RetrievalReceipt {
            generation: "matrix".into(),
            lexical_count: 8,
            vector_count: 8,
            lexical_truncated: false,
            vector_truncated: false,
            fusion_truncated: 0,
            relation_truncated: 0,
            candidates: (0..8)
                .map(|n| RetrievedCapability {
                    id: format!("c{n}"),
                    reference: CapabilityRef {
                        catalog: "matrix".into(),
                        capability: format!("op{n}"),
                    },
                    document: CapabilityDocument {
                        capability: format!("op{n}"),
                        entity: "Record".into(),
                        text: "Abstract fixture operation".into(),
                        text_hash: String::new(),
                        related_entities: vec![],
                    },
                    admissions: BTreeSet::new(),
                })
                .collect(),
        }
    }

    fn decode(
        value: serde_json::Value,
        receipt: &RetrievalReceipt,
    ) -> Result<(CapabilitySelection, Vec<CapabilityRef>)> {
        let raw =
            json!({"choices":[{"finish_reason":"stop","message":{"content":value.to_string()}}]})
                .to_string();
        selector_contract::decode(&raw, "inspect records and synthesize music", receipt)
    }

    #[test]
    fn zero_additions_and_many_roots_are_valid() {
        for count in [0, 1, 4, 8] {
            let ids: Vec<_> = (0..count).map(|n| format!("c{n}")).collect();
            let (selection, business) = decode(
                json!({"additional_capability_ids":ids,"unsupported":[]}),
                &retrieval(),
            )
            .unwrap();
            assert_eq!(selection.status, SelectionStatus::Ready);
            assert_eq!(business.len(), count);
        }
    }

    #[test]
    fn unsupported_preserves_supported_work_but_never_returns_ready() {
        let value = json!({"additional_capability_ids":["c0"],"unsupported":[{"intent_quote":"synthesize music","reason":"No supplied capability"}]});
        let (selection, business) = decode(value.clone(), &retrieval()).unwrap();
        assert_eq!(selection.status, SelectionStatus::Insufficient);
        assert_eq!(business.len(), 1);
        for flag in 0..4 {
            let mut cut = retrieval();
            match flag {
                0 => cut.lexical_truncated = true,
                1 => cut.vector_truncated = true,
                2 => cut.fusion_truncated = 1,
                _ => cut.relation_truncated = 1,
            }
            let (selection, business) = decode(value.clone(), &cut).unwrap();
            assert_eq!(selection.status, SelectionStatus::Insufficient);
            assert_eq!(business.len(), 1);
        }
    }

    #[test]
    fn malformed_and_abolished_contracts_are_rejected() {
        for value in [
            json!({"requirements":[]}),
            json!({"additional_capability_ids":["unknown"],"unsupported":[]}),
            json!({"additional_capability_ids":["c0","c0"],"unsupported":[]}),
            json!({"additional_capability_ids":[],"unsupported":[{"intent_quote":"absent text","reason":"Unavailable"}]}),
            json!({"additional_capability_ids":[],"unsupported":[{"intent_quote":"records","reason":""}]}),
            json!({"additional_capability_ids":[],"unsupported":[],"status":"ready"}),
        ] {
            assert!(decode(value, &retrieval()).is_err());
        }
    }

    #[test]
    fn wire_schema_matches_offline_oracle() {
        // Embedded abstract wire fixture; no production API catalog dependency.
        let expected = json!({"type":"object","additionalProperties":false,"required":["additional_capability_ids","unsupported"],"properties":{
            "additional_capability_ids":{"type":"array","items":{"type":"string"}},
            "unsupported":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["intent_quote","reason"],"properties":{"intent_quote":{"type":"string"},"reason":{"type":"string"}}}}
        }});
        assert_eq!(selector_contract::schema(), expected);
    }
}
