//! Score frozen receipts without retaining an alternate discovery implementation.

use plasm_core::prerequisites::CapabilityRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

pub fn score(case: FrozenCase) -> anyhow::Result<CaseScore> {
    let status = case
        .receipt
        .pointer("/selection/status")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("{}: receipt missing selection status", case.id))?;
    anyhow::ensure!(
        ["ready", "insufficient"].contains(&status),
        "invalid routing status"
    );
    let closure = case.receipt.get("closure").filter(|value| !value.is_null());
    anyhow::ensure!(
        status != "ready" || closure.is_some(),
        "Ready must have a closure"
    );
    let read = |field: &str| -> anyhow::Result<BTreeSet<CapabilityRef>> {
        match closure {
            Some(closure) => Ok(serde_json::from_value(
                closure
                    .get(field)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("closure missing {field}"))?,
            )?),
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
