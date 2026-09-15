//! Capability sufficiency validation; conversational decisions belong to the agent.
use crate::discovery_store::RetrievalReceipt;
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

/// Host-facing unresolved clause (derived from requirement coverage).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedWork {
    /// Selector-authored semantic requirement description. This is explanatory
    /// text, not an address and is never matched against the original intent.
    pub requirement: String,
    pub reason: String,
}

/// Explicit per-requirement association: supporting selected∪exposed IDs **or** an unresolved reason.
///
/// This makes omissions auditable. It is **not** deterministic semantic proof —
/// the model still judges which clauses exist and which IDs support them.
/// Deterministic check: each supporting ID must be selected or already exposed.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementCoverage {
    /// Concise semantic description of the requirement being assessed.
    /// The selector need not copy any substring from the request.
    pub requirement: String,
    /// Offered candidate IDs that support this requirement. Empty when unresolved.
    /// Validation requires each ID to be **selected** or **already exposed**
    /// (not merely offered on the candidate slate).
    pub supporting_capability_ids: Vec<String>,
    /// Nonempty when unresolved within the presented universe; empty when covered.
    #[serde(default)]
    pub unresolved_reason: String,
}

impl RequirementCoverage {
    pub fn is_unresolved(&self) -> bool {
        !self.unresolved_reason.trim().is_empty()
    }

    pub fn as_unsupported(&self) -> Option<UnsupportedWork> {
        if self.is_unresolved() {
            Some(UnsupportedWork {
                requirement: self.requirement.clone(),
                reason: self.unresolved_reason.clone(),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySelection {
    /// Model-reported sufficiency of presented capabilities, never permission to execute.
    pub status: SelectionStatus,
    pub additional_capability_ids: Vec<String>,
    /// Every identified requirement, each with supporting IDs or an unresolved reason.
    pub requirement_coverage: Vec<RequirementCoverage>,
}

impl CapabilitySelection {
    pub fn from_capabilities(
        additional_capability_ids: Vec<String>,
        requirement_coverage: Vec<RequirementCoverage>,
        retrieval: &RetrievalReceipt,
    ) -> Result<Self> {
        let has_unresolved = requirement_coverage
            .iter()
            .any(RequirementCoverage::is_unresolved);
        let selection = Self {
            status: if has_unresolved {
                SelectionStatus::Insufficient
            } else {
                SelectionStatus::Ready
            },
            additional_capability_ids,
            requirement_coverage,
        };
        validate_selection(&selection, retrieval)?;
        Ok(selection)
    }

    /// Unresolved clauses derived from coverage (host recovery / explanation surface).
    pub fn unresolved(&self) -> Vec<UnsupportedWork> {
        self.requirement_coverage
            .iter()
            .filter_map(RequirementCoverage::as_unsupported)
            .collect()
    }

    pub fn explanation_lines(&self) -> Vec<String> {
        self.unresolved()
            .into_iter()
            .map(|work| format!("Unresolved: {} — {}", work.requirement, work.reason))
            .collect()
    }
}

pub(crate) fn validate_selection(
    selection: &CapabilitySelection,
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
    let expected = if selection
        .requirement_coverage
        .iter()
        .any(RequirementCoverage::is_unresolved)
    {
        SelectionStatus::Insufficient
    } else {
        SelectionStatus::Ready
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
    let selected_ids: BTreeSet<&str> = selection
        .additional_capability_ids
        .iter()
        .map(String::as_str)
        .collect();
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
    // Witnesses must be executable: selected this turn ∪ already exposed.
    // Offered-but-unselected IDs must not validate coverage — execution closure
    // is built from selected (+ prior exposure), not the full candidate slate.
    let exposed_ids: BTreeSet<&str> = retrieval
        .candidates
        .iter()
        .filter(|c| c.admissions.contains("already_exposed"))
        .map(|c| c.id.as_str())
        .collect();
    for coverage in &selection.requirement_coverage {
        if coverage.requirement.trim().is_empty() {
            bail!("requirement coverage requires a nonempty semantic description");
        }
        let supporting_nonempty = !coverage.supporting_capability_ids.is_empty();
        let unresolved_nonempty = coverage.is_unresolved();
        match (supporting_nonempty, unresolved_nonempty) {
            (true, false) => {}
            (false, true) => {}
            (true, true) => {
                bail!(
                    "requirement coverage must associate supporting IDs or an unresolved reason, not both"
                )
            }
            (false, false) => {
                bail!("requirement coverage must associate supporting IDs or an unresolved reason")
            }
        }
        if coverage
            .supporting_capability_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != coverage.supporting_capability_ids.len()
        {
            bail!("duplicate supporting capability ID in requirement coverage");
        }
        for id in &coverage.supporting_capability_ids {
            if !candidates.contains_key(id.as_str()) {
                bail!("requirement coverage references an unknown candidate ID");
            }
            if !selected_ids.contains(id.as_str()) && !exposed_ids.contains(id.as_str()) {
                bail!("requirement coverage supporting ID is not selected or already exposed");
            }
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
        selector_contract::decode(&raw, receipt)
    }

    fn covered(quote: &str, ids: &[&str]) -> serde_json::Value {
        json!({
            "requirement": quote,
            "supporting_capability_ids": ids,
            "unresolved_reason": ""
        })
    }

    fn unresolved(quote: &str, reason: &str) -> serde_json::Value {
        json!({
            "requirement": quote,
            "supporting_capability_ids": [],
            "unresolved_reason": reason
        })
    }

    #[test]
    fn zero_additions_and_many_roots_are_valid() {
        for count in [0, 1, 4, 8] {
            let ids: Vec<_> = (0..count).map(|n| format!("c{n}")).collect();
            let coverage = if count == 0 {
                json!([])
            } else {
                let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                json!([covered("inspect records", &id_refs)])
            };
            let (selection, business) = decode(
                json!({"additional_capability_ids":ids,"requirement_coverage":coverage}),
                &retrieval(),
            )
            .unwrap();
            assert_eq!(selection.status, SelectionStatus::Ready);
            assert_eq!(business.len(), count);
        }
    }

    #[test]
    fn unresolved_preserves_supported_work_but_never_returns_ready() {
        let value = json!({
            "additional_capability_ids":["c0"],
            "requirement_coverage":[
                covered("inspect records", &["c0"]),
                unresolved("synthesize music", "No supplied capability")
            ]
        });
        let (selection, business) = decode(value.clone(), &retrieval()).unwrap();
        assert_eq!(selection.status, SelectionStatus::Insufficient);
        assert_eq!(business.len(), 1);
        assert_eq!(selection.unresolved().len(), 1);
        let lines = selection.explanation_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Unresolved:"), "{}", lines[0]);
        assert!(!lines[0].starts_with("Unsupported:"));
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
    fn composite_missing_clause_is_auditable_in_coverage() {
        let value = json!({
            "additional_capability_ids":["c0"],
            "requirement_coverage":[
                covered("inspect records", &["c0"]),
                unresolved("synthesize music", "Presented documents do not expose that operation")
            ]
        });
        let (selection, _) = decode(value, &retrieval()).unwrap();
        assert_eq!(selection.requirement_coverage.len(), 2);
        assert!(!selection.requirement_coverage[0].is_unresolved());
        assert!(selection.requirement_coverage[1].is_unresolved());
        assert_eq!(
            selection.requirement_coverage[0].supporting_capability_ids,
            vec!["c0".to_string()]
        );
    }

    #[test]
    fn malformed_and_abolished_contracts_are_rejected() {
        for value in [
            json!({"requirements":[]}),
            json!({"additional_capability_ids":["unknown"],"requirement_coverage":[]}),
            json!({"additional_capability_ids":["c0","c0"],"requirement_coverage":[]}),
            json!({"additional_capability_ids":[],"requirement_coverage":[unresolved("records","")]}),
            json!({"additional_capability_ids":[],"requirement_coverage":[],"status":"ready"}),
            // abolished dual shape
            json!({"additional_capability_ids":[],"unsupported":[]}),
            json!({
                "additional_capability_ids":["c0"],
                "requirement_coverage":[{
                    "requirement":"inspect records",
                    "supporting_capability_ids":["c0"],
                    "unresolved_reason":"also unresolved"
                }]
            }),
            json!({
                "additional_capability_ids":[],
                "requirement_coverage":[{
                    "requirement":"inspect records",
                    "supporting_capability_ids":[],
                    "unresolved_reason":""
                }]
            }),
        ] {
            assert!(decode(value, &retrieval()).is_err());
        }
    }

    #[test]
    fn semantic_requirement_description_need_not_quote_intent() {
        let (selection, _) = decode(
            json!({
                "additional_capability_ids":[],
                "requirement_coverage":[unresolved(
                    "compose an audio representation",
                    "No supplied capability"
                )]
            }),
            &retrieval(),
        )
        .unwrap();
        assert_eq!(selection.status, SelectionStatus::Insufficient);
        assert_eq!(
            selection.requirement_coverage[0].requirement,
            "compose an audio representation"
        );
    }

    #[test]
    fn invented_offered_hash_capability_id_is_rejected() {
        let revision = "9fd034a4069d217f31eb5871b716bb38dac86f013a312f94ad14b6820a99cd7f";
        let receipt = hash_shaped_retrieval(revision, &["transaction_query", "transaction_get"]);
        let invented = format!("{revision}/payment_query");
        let error = decode(
            json!({"additional_capability_ids":[invented],"requirement_coverage":[]}),
            &receipt,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown candidate ID"),
            "{error:#}"
        );
    }

    #[test]
    fn offered_hash_capability_id_is_accepted() {
        let revision = "9fd034a4069d217f31eb5871b716bb38dac86f013a312f94ad14b6820a99cd7f";
        let receipt = hash_shaped_retrieval(revision, &["transaction_query", "transaction_get"]);
        let offered = format!("{revision}/transaction_query");
        let (selection, business) = decode(
            json!({
                "additional_capability_ids":[offered.clone()],
                "requirement_coverage":[covered("inspect records", &[offered.as_str()])]
            }),
            &receipt,
        )
        .unwrap();
        assert_eq!(selection.status, SelectionStatus::Ready);
        assert_eq!(business.len(), 1);
        assert_eq!(
            business[0],
            CapabilityRef {
                catalog: "matrix".into(),
                capability: "transaction_query".into(),
            }
        );
    }

    #[test]
    fn already_exposed_candidate_id_stays_selectable() {
        let mut receipt = retrieval();
        receipt
            .candidates
            .first_mut()
            .unwrap()
            .admissions
            .insert("already_exposed".into());
        let (selection, business) = decode(
            json!({
                "additional_capability_ids":["c0"],
                "requirement_coverage":[covered("inspect records", &["c0"])]
            }),
            &receipt,
        )
        .unwrap();
        assert_eq!(selection.status, SelectionStatus::Ready);
        assert_eq!(business.len(), 1);
        assert_eq!(selection.additional_capability_ids, vec!["c0".to_string()]);
    }

    #[test]
    fn supporting_id_not_in_selected_or_exposed_is_rejected() {
        // Selector picks c0 but claims coverage via offered-but-unselected c1.
        let error = decode(
            json!({
                "additional_capability_ids":["c0"],
                "requirement_coverage":[covered("inspect records", &["c1"])]
            }),
            &retrieval(),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("not selected or already exposed"),
            "{error:#}"
        );
    }

    #[test]
    fn supporting_id_may_cite_already_exposed_without_reselecting() {
        let mut receipt = retrieval();
        receipt
            .candidates
            .first_mut()
            .unwrap()
            .admissions
            .insert("already_exposed".into());
        let (selection, business) = decode(
            json!({
                "additional_capability_ids":[],
                "requirement_coverage":[covered("inspect records", &["c0"])]
            }),
            &receipt,
        )
        .unwrap();
        assert_eq!(selection.status, SelectionStatus::Ready);
        assert!(business.is_empty());
        assert_eq!(
            selection.requirement_coverage[0].supporting_capability_ids,
            vec!["c0".to_string()]
        );
    }

    #[test]
    fn already_exposed_ref_is_not_a_decode_id() {
        let error = decode(
            json!({"additional_capability_ids":["matrix/op0"],"requirement_coverage":[]}),
            &retrieval(),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown candidate ID"),
            "{error:#}"
        );
    }

    #[test]
    fn wire_schema_is_closed_over_offered_ids() {
        let receipt = retrieval();
        let offered = selector_contract::offered_ids(&receipt);
        let expected = json!({"type":"object","additionalProperties":false,"required":["additional_capability_ids","requirement_coverage"],"properties":{
            "additional_capability_ids":{"type":"array","items":{"type":"string","enum":offered.clone()}},
            "requirement_coverage":{"type":"array","items":{"type":"object","additionalProperties":false,
                "required":["requirement","supporting_capability_ids","unresolved_reason"],
                "properties":{
                    "requirement":{"type":"string","minLength":1},
                    "supporting_capability_ids":{"type":"array","items":{"type":"string","enum":offered}},
                    "unresolved_reason":{"type":"string"}
                }}}
        }});
        assert_eq!(selector_contract::schema(&receipt), expected);
        assert_eq!(
            selector_contract::offered_ids(&receipt),
            (0..8).map(|n| format!("c{n}")).collect::<Vec<_>>()
        );
        let mut empty = retrieval();
        empty.candidates.clear();
        assert_eq!(
            selector_contract::schema(&empty)["properties"]["additional_capability_ids"]["items"]
                ["enum"],
            json!([])
        );
        assert!(decode(
            json!({"additional_capability_ids":[],"requirement_coverage":[]}),
            &empty
        )
        .is_ok());
        assert!(decode(
            json!({"additional_capability_ids":["c0"],"requirement_coverage":[]}),
            &empty
        )
        .is_err());
    }

    fn hash_shaped_retrieval(revision: &str, capabilities: &[&str]) -> RetrievalReceipt {
        RetrievalReceipt {
            generation: "matrix".into(),
            lexical_count: capabilities.len(),
            vector_count: capabilities.len(),
            lexical_truncated: false,
            vector_truncated: false,
            fusion_truncated: 0,
            relation_truncated: 0,
            candidates: capabilities
                .iter()
                .map(|capability| RetrievedCapability {
                    id: format!("{revision}/{capability}"),
                    reference: CapabilityRef {
                        catalog: "matrix".into(),
                        capability: (*capability).to_string(),
                    },
                    document: CapabilityDocument {
                        capability: (*capability).to_string(),
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
}
