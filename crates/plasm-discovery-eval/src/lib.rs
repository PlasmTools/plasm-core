//! Score frozen receipts without retaining an alternate discovery implementation.

use plasm_core::prerequisites::CapabilityRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryScoreError {
    #[error("receipt {case_id} is missing selection.status")]
    MissingSelectionStatus { case_id: String },
    #[error("receipt has an unsupported routing status")]
    InvalidRoutingStatus,
    #[error("ready routing receipt must include a closure")]
    ReadyWithoutClosure,
    #[error("routing closure is missing `{field}`")]
    MissingClosureField { field: &'static str },
    #[error("routing closure `{field}` has an invalid shape")]
    InvalidClosureField {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenCase {
    pub id: String,
    pub expected_business: BTreeSet<CapabilityRef>,
    pub expected_prerequisites: BTreeSet<CapabilityRef>,
    pub allowed: BTreeSet<CapabilityRef>,
    pub receipt: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct CaseScore {
    pub id: String,
    pub status: String,
    pub missing_business: BTreeSet<CapabilityRef>,
    pub missing_prerequisites: BTreeSet<CapabilityRef>,
    pub excess_business: BTreeSet<CapabilityRef>,
    pub unauthorized: BTreeSet<CapabilityRef>,
    pub false_ready: bool,
}

pub fn score(case: FrozenCase) -> Result<CaseScore, DiscoveryScoreError> {
    let status = case
        .receipt
        .pointer("/selection/status")
        .and_then(|value| value.as_str())
        .ok_or_else(|| DiscoveryScoreError::MissingSelectionStatus {
            case_id: case.id.clone(),
        })?;
    if !["ready", "insufficient"].contains(&status) {
        return Err(DiscoveryScoreError::InvalidRoutingStatus);
    }
    let closure = case.receipt.get("closure").filter(|value| !value.is_null());
    if status == "ready" && closure.is_none() {
        return Err(DiscoveryScoreError::ReadyWithoutClosure);
    }
    let read = |field: &'static str| -> Result<BTreeSet<CapabilityRef>, DiscoveryScoreError> {
        match closure {
            Some(closure) => Ok(serde_json::from_value(
                closure
                    .get(field)
                    .cloned()
                    .ok_or(DiscoveryScoreError::MissingClosureField { field })?,
            )
            .map_err(|source| DiscoveryScoreError::InvalidClosureField { field, source })?),
            None => Ok(BTreeSet::new()),
        }
    };
    let business = read("business")?;
    let prerequisites = read("prerequisites")?;
    let missing_business = case
        .expected_business
        .difference(&business)
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing_prerequisites = case
        .expected_prerequisites
        .difference(&prerequisites)
        .cloned()
        .collect::<BTreeSet<_>>();
    let unauthorized = business
        .union(&prerequisites)
        .filter(|cap| !case.allowed.contains(*cap))
        .cloned()
        .collect::<BTreeSet<_>>();
    let false_ready = status == "ready"
        && (!missing_business.is_empty()
            || !missing_prerequisites.is_empty()
            || !unauthorized.is_empty());
    Ok(CaseScore {
        id: case.id,
        status: status.to_string(),
        missing_business,
        missing_prerequisites,
        excess_business: business
            .difference(&case.expected_business)
            .cloned()
            .collect(),
        unauthorized,
        false_ready,
    })
}
