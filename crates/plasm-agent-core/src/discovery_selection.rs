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

/// Requirement support or a specific gap, with useful capabilities preserved.
///
/// This makes omissions auditable. It is **not** deterministic semantic proof —
/// the model still judges which clauses exist and which IDs support them.
/// Assessment IDs are the sole choice; the host derives additions excluding prior exposure.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementCoverage {
    /// Concise semantic description of the requirement being assessed.
    /// The selector need not copy any substring from the request.
    pub requirement: String,
    pub assessment: RequirementAssessment,
}

/// Disjoint wire shapes prevent a missing-input claim from masquerading as support.
/// An unresolved requirement may still expose useful consumers and producers.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
pub enum RequirementAssessment {
    NoCapabilityNeeded {
        no_capability_needed: String,
    },
    Supported {
        supported_by: Vec<String>,
    },
    Unresolved {
        useful_capabilities: Vec<String>,
        missing: String,
    },
}

impl RequirementAssessment {
    pub fn capability_ids(&self) -> &[String] {
        match self {
            Self::NoCapabilityNeeded { .. } => &[],
            Self::Supported { supported_by } => supported_by,
            Self::Unresolved {
                useful_capabilities,
                ..
            } => useful_capabilities,
        }
    }

    pub(crate) fn capability_ids_mut(&mut self) -> &mut [String] {
        match self {
            Self::NoCapabilityNeeded { .. } => &mut [],
            Self::Supported { supported_by } => supported_by,
            Self::Unresolved {
                useful_capabilities,
                ..
            } => useful_capabilities,
        }
    }

    pub fn missing(&self) -> Option<&str> {
        match self {
            Self::Supported { .. } | Self::NoCapabilityNeeded { .. } => None,
            Self::Unresolved { missing, .. } => Some(missing),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::NoCapabilityNeeded {
                no_capability_needed,
            } if no_capability_needed.trim().is_empty() => bail!("blank local assessment"),
            Self::Supported { supported_by } if supported_by.is_empty() => {
                bail!("supported assessment requires capability evidence")
            }
            Self::Unresolved { missing, .. } if missing.trim().is_empty() => {
                bail!("unresolved assessment requires a specific missing capability or input")
            }
            _ => Ok(()),
        }
    }
}

impl RequirementCoverage {
    pub fn is_unresolved(&self) -> bool {
        self.assessment.missing().is_some()
    }

    pub fn as_unsupported(&self) -> Option<UnsupportedWork> {
        self.assessment.missing().map(|missing| UnsupportedWork {
            requirement: self.requirement.clone(),
            reason: missing.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySelection {
    /// Host-derived sufficiency of presented capabilities, never permission to execute.
    pub status: SelectionStatus,
    /// Host-derived additions; never authored separately by the selector.
    pub additional_capability_ids: Vec<String>,
    /// Every identified requirement, with support or a gap and useful capabilities.
    pub requirement_coverage: Vec<RequirementCoverage>,
}

impl CapabilitySelection {
    pub fn from_coverage(
        requirement_coverage: Vec<RequirementCoverage>,
        retrieval: &RetrievalReceipt,
    ) -> Result<Self> {
        let has_unresolved = requirement_coverage
            .iter()
            .any(RequirementCoverage::is_unresolved);
        let exposed: BTreeSet<_> = retrieval
            .candidates
            .iter()
            .filter(|c| c.admissions.contains("already_exposed"))
            .map(|c| c.id.as_str())
            .collect();
        let additional_capability_ids = requirement_coverage
            .iter()
            .flat_map(|r| r.assessment.capability_ids().iter())
            .filter(|id| !exposed.contains(id.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
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
        coverage.assessment.validate()?;
        let evidence_ids = coverage.assessment.capability_ids();
        if evidence_ids.iter().collect::<BTreeSet<_>>().len() != evidence_ids.len() {
            bail!("duplicate supporting capability ID in requirement coverage");
        }
        for id in evidence_ids {
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
                    id: format!("revision-{n}/op{n}"),
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
    fn covered(requirement: &str, ids: &[&str]) -> serde_json::Value {
        json!({"requirement":requirement,"assessment":{"supported_by":ids}})
    }
    fn unresolved(requirement: &str) -> serde_json::Value {
        json!({"requirement":requirement,"assessment":{"useful_capabilities":[],"missing":"No supplied capability"}})
    }
    fn partial(requirement: &str, ids: &[&str]) -> serde_json::Value {
        json!({"requirement":requirement,"assessment":{"useful_capabilities":ids,"missing":"Required input acquisition is not supported"}})
    }
    fn decode(
        value: serde_json::Value,
        retrieval: &RetrievalReceipt,
    ) -> Result<(CapabilitySelection, Vec<CapabilityRef>)> {
        selector_contract::decode(
            &json!({"choices":[{"finish_reason":"stop","message":{"content":value.to_string()}}]})
                .to_string(),
            retrieval,
        )
    }
    #[test]
    fn coverage_is_the_only_selection_and_many_roots_are_valid() {
        for count in [0, 1, 4, 8] {
            let ids: Vec<_> = (0..count).map(|n| format!("c{n}")).collect();
            let refs: Vec<_> = ids.iter().map(String::as_str).collect();
            let coverage = if count == 0 {
                json!([])
            } else {
                json!([covered("inspect records", &refs)])
            };
            let (selection, business) =
                decode(json!({"requirement_coverage":coverage}), &retrieval()).unwrap();
            assert_eq!(selection.status, SelectionStatus::Ready);
            assert_eq!(business.len(), count);
            assert_eq!(selection.additional_capability_ids.len(), count);
        }
    }
    #[test]
    fn offered_supporting_id_is_selected_without_a_second_list() {
        let (selection, business) = decode(
            json!({"requirement_coverage":[covered("read records", &["c1"])]}),
            &retrieval(),
        )
        .unwrap();
        assert_eq!(selection.additional_capability_ids, vec!["revision-1/op1"]);
        assert_eq!(
            business,
            vec![CapabilityRef {
                catalog: "matrix".into(),
                capability: "op1".into()
            }]
        );
    }
    #[test]
    fn already_exposed_witness_needs_no_addition() {
        let mut receipt = retrieval();
        receipt.candidates[0]
            .admissions
            .insert("already_exposed".into());
        let (selection, business) = decode(
            json!({"requirement_coverage":[covered("read records", &["c0"])]}),
            &receipt,
        )
        .unwrap();
        assert!(selection.additional_capability_ids.is_empty());
        assert!(business.is_empty());
        assert_eq!(
            selection.requirement_coverage[0]
                .assessment
                .capability_ids(),
            &["revision-0/op0"]
        );
    }
    #[test]
    fn reused_capability_is_selected_once_across_requirements() {
        let (selection,business)=decode(json!({"requirement_coverage":[covered("read records", &["c0"]),covered("rank records", &["c0"])]}),&retrieval()).unwrap();
        assert_eq!(selection.additional_capability_ids.len(), 1);
        assert_eq!(business.len(), 1);
    }
    #[test]
    fn partial_requirement_retains_its_consumer_without_claiming_support() {
        let (selection, business) = decode(
            json!({"requirement_coverage":[partial("perform requested effect", &["c1"])]}),
            &retrieval(),
        )
        .unwrap();
        assert_eq!(selection.status, SelectionStatus::Insufficient);
        assert_eq!(selection.additional_capability_ids, vec!["revision-1/op1"]);
        assert_eq!(business.len(), 1);
        assert_eq!(
            selection.unresolved()[0].reason,
            "Required input acquisition is not supported"
        );
    }

    proptest::proptest! {
        #[test]
        fn typed_assessments_preserve_selection_gaps_and_cache_across_permutations(
            assessments in proptest::collection::vec((proptest::bool::ANY, 0u8..8), 1..32),
            exposed_mask in proptest::num::u8::ANY,
            rotation in 0usize..8,
        ) {
            let mut receipt = retrieval();
            for (n, candidate) in receipt.candidates.iter_mut().enumerate() {
                if exposed_mask & (1 << n) != 0 {
                    candidate.admissions.insert("already_exposed".into());
                }
            }
            receipt.candidates.rotate_left(rotation);
            let coverage: Vec<_> = assessments.iter().enumerate().map(|(n, (gap, id))| {
                let requirement = format!("abstract outcome {n}");
                let alias = format!("c{id}");
                if *gap { partial(&requirement, &[&alias]) } else { covered(&requirement, &[&alias]) }
            }).collect();
            let (selection, _) = decode(json!({"requirement_coverage": coverage}), &receipt).unwrap();
            let expected_ids: Vec<_> = assessments.iter()
                .map(|(_, id)| *id)
                .filter(|id| exposed_mask & (1 << id) == 0)
                .map(|id| format!("revision-{id}/op{id}"))
                .collect::<BTreeSet<_>>().into_iter().collect();
            proptest::prop_assert_eq!(&selection.additional_capability_ids, &expected_ids);
            proptest::prop_assert_eq!(selection.status == SelectionStatus::Insufficient, assessments.iter().any(|(gap, _)| *gap));
            let envelope = selector_contract::selection_envelope(&selection, &receipt).unwrap();
            let (cached, _) = selector_contract::decode(&selector_contract::wrap_cached_envelope(&envelope), &receipt).unwrap();
            proptest::prop_assert_eq!(&selection, &cached);
            receipt.candidates.reverse();
            let (permuted, _) = selector_contract::decode(&selector_contract::wrap_cached_envelope(&envelope), &receipt).unwrap();
            proptest::prop_assert_eq!(selection, permuted);
        }
    }
    #[test]
    fn unresolved_preserves_supported_work_under_retrieval_truncation() {
        for flag in 0..4 {
            let mut receipt = retrieval();
            match flag {
                0 => receipt.lexical_truncated = true,
                1 => receipt.vector_truncated = true,
                2 => receipt.fusion_truncated = 1,
                _ => receipt.relation_truncated = 1,
            }
            let (selection,business)=decode(json!({"requirement_coverage":[covered("consume input", &["c0"]),unresolved("acquire missing input")]}),&receipt).unwrap();
            assert_eq!(selection.status, SelectionStatus::Insufficient);
            assert_eq!(business.len(), 1);
            assert_eq!(selection.unresolved().len(), 1);
            assert!(selection.explanation_lines()[0].starts_with("Unresolved:"));
        }
    }
    #[test]
    fn semantic_requirement_description_need_not_quote_intent() {
        let (selection, _) = decode(
            json!({"requirement_coverage":[unresolved("compose an audio representation")]}),
            &retrieval(),
        )
        .unwrap();
        assert_eq!(
            selection.requirement_coverage[0].requirement,
            "compose an audio representation"
        );
    }
    #[test]
    fn malformed_and_abolished_contracts_are_rejected() {
        for value in [
            json!({"requirements":[]}),
            json!({"additional_capability_ids":[],"requirement_coverage":[]}),
            json!({"requirement_coverage":[],"status":"ready"}),
            json!({"requirement_coverage":[covered("read records",&[])]}),
            json!({"requirement_coverage":[covered("read records",&["c0","c0"])]}),
            json!({"requirement_coverage":[{"requirement":"read","supporting_capability_ids":["c0"],"unresolved_reason":"also unresolved"}]}),
            json!({"requirement_coverage":[{"requirement":"read","assessment":{"supported_by":["c0"],"missing":"also unresolved","useful_capabilities":["c0"]}}]}),
            json!({"requirement_coverage":[{"requirement":"read","assessment":{"useful_capabilities":[],"missing":"  "}}]}),
            json!({"requirement_coverage":[partial("read", &["c0","c0"])]}),
            json!({"requirement_coverage":[covered("",&["c0"])]}),
        ] {
            assert!(decode(value, &retrieval()).is_err());
        }
    }
    #[test]
    fn unknown_alias_and_canonical_reference_are_rejected() {
        for id in ["c99", "revision-0/op0", "matrix/op0"] {
            assert!(decode(
                json!({"requirement_coverage":[partial("consume records", &[id])]}),
                &retrieval()
            )
            .is_err());
            let error = decode(
                json!({"requirement_coverage":[covered("read records",&[id])]}),
                &retrieval(),
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("unknown candidate ID"));
        }
    }
    #[test]
    fn canonical_revision_id_round_trips_through_short_alias() {
        let mut receipt = retrieval();
        receipt.candidates.truncate(1);
        receipt.candidates[0].id = format!("{}/op0", "a".repeat(64));
        let (selection, _) = decode(
            json!({"requirement_coverage":[covered("read records",&["c0"])]}),
            &receipt,
        )
        .unwrap();
        assert_eq!(
            selection.additional_capability_ids,
            vec![receipt.candidates[0].id.clone()]
        );
        let envelope = selector_contract::selection_envelope(&selection, &receipt).unwrap();
        assert!(!envelope.contains(&receipt.candidates[0].id));
        let (cached, _) = selector_contract::decode(
            &selector_contract::wrap_cached_envelope(&envelope),
            &receipt,
        )
        .unwrap();
        assert_eq!(selection.requirement_coverage, cached.requirement_coverage);
        assert_eq!(
            selection.additional_capability_ids,
            cached.additional_capability_ids
        );
    }
    #[test]
    fn wire_schema_is_closed_over_request_local_ids_only() {
        let receipt = retrieval();
        let schema = selector_contract::schema(&receipt);
        assert_eq!(schema["required"], json!(["requirement_coverage"]));
        assert_eq!(
            schema["properties"]["requirement_coverage"]["items"]["properties"]["assessment"]
                ["anyOf"][0]["properties"]["supported_by"]["items"]["enum"],
            json!((0..8).map(|n| format!("c{n}")).collect::<Vec<_>>())
        );
        assert!(schema["properties"]
            .get("additional_capability_ids")
            .is_none());
        let mut empty = retrieval();
        empty.candidates.clear();
        let empty_schema = selector_contract::schema(&empty);
        let assessments = &empty_schema["properties"]["requirement_coverage"]["items"]
            ["properties"]["assessment"]["anyOf"];
        assert_eq!(assessments.as_array().unwrap().len(), 1);
        assert_eq!(
            assessments[0]["properties"]["useful_capabilities"]["maxItems"],
            json!(0)
        );
        assert!(decode(
            json!({"requirement_coverage":[unresolved("acquire unavailable input")]}),
            &empty
        )
        .is_ok());
        assert!(decode(json!({"requirement_coverage":[]}), &empty).is_ok());
        assert!(decode(
            json!({"requirement_coverage":[covered("read",&["c0"])]}),
            &empty
        )
        .is_err());
    }
}
