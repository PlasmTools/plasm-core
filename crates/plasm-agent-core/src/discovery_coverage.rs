//! Persistent capability coverage, distinct from execution or task completion.
use crate::discovery_matcher::{CapabilityMatchReceipt, EffectSlot, MatchChoice};
use crate::discovery_store::RetrievalReceipt;
use anyhow::{ensure, Context, Result};
use plasm_core::prerequisites::CapabilityRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryObligation {
    pub slot: EffectSlot,
    pub matched_capabilities: BTreeSet<CapabilityRef>,
}

/// Statements are append-only and matches are witnesses of capability applicability,
/// not permission to execute without the constraints in intent provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireCoverage")]
pub struct DiscoveryCoverage {
    obligations: Vec<DiscoveryObligation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCoverage {
    obligations: Vec<DiscoveryObligation>,
}

impl TryFrom<WireCoverage> for DiscoveryCoverage {
    type Error = anyhow::Error;
    fn try_from(wire: WireCoverage) -> Result<Self> {
        let mut statements = BTreeSet::new();
        for (index, obligation) in wire.obligations.iter().enumerate() {
            ensure!(
                obligation.slot.id == format!("s{index}"),
                "coverage slot identity changed"
            );
            ensure!(
                !obligation.slot.statement.trim().is_empty()
                    && !obligation.slot.statement.contains('\0')
                    && statements.insert(&obligation.slot.statement),
                "coverage statements must be nonempty, unique and without NUL"
            );
        }
        Ok(Self {
            obligations: wire.obligations,
        })
    }
}

impl DiscoveryCoverage {
    pub fn obligations(&self) -> &[DiscoveryObligation] {
        &self.obligations
    }

    pub fn matched_capabilities(&self) -> Vec<CapabilityRef> {
        self.obligations
            .iter()
            .flat_map(|entry| &entry.matched_capabilities)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &EffectSlot> {
        self.obligations
            .iter()
            .filter(|entry| entry.matched_capabilities.is_empty())
            .map(|entry| &entry.slot)
    }

    /// Retrieve against the caller's current needs, but judge recalled candidates
    /// against unresolved earlier needs too. An omitted statement never disappears.
    pub fn slots_for_turn(&mut self, current: &[String]) -> Result<Vec<EffectSlot>> {
        ensure!(
            (1..=64).contains(&current.len()),
            "one to 64 current effect slots required"
        );
        for statement in current {
            ensure!(
                !statement.trim().is_empty() && !statement.contains('\0'),
                "invalid discovery slot"
            );
            if !self
                .obligations
                .iter()
                .any(|entry| &entry.slot.statement == statement)
            {
                self.obligations.push(DiscoveryObligation {
                    slot: EffectSlot {
                        id: format!("s{}", self.obligations.len()),
                        statement: statement.clone(),
                    },
                    matched_capabilities: BTreeSet::new(),
                });
            }
        }
        Ok(self
            .obligations
            .iter()
            .filter(|entry| {
                entry.matched_capabilities.is_empty() || current.contains(&entry.slot.statement)
            })
            .map(|entry| entry.slot.clone())
            .collect())
    }

    pub fn observe(
        &mut self,
        matching: &CapabilityMatchReceipt,
        retrieval: &RetrievalReceipt,
    ) -> Result<()> {
        for slot in &matching.slots {
            ensure!(
                self.obligations.iter().any(|entry| &entry.slot == slot),
                "matching changed coverage slot"
            );
        }
        for matched in &matching.matches {
            if matched.choice != MatchChoice::DirectMatch {
                continue;
            }
            let reference = &retrieval
                .candidates
                .iter()
                .find(|candidate| candidate.id == matched.capability_id)
                .context("coverage match has no retrieved candidate")?
                .reference;
            let obligation = self
                .obligations
                .iter_mut()
                .find(|entry| entry.slot.id == matched.slot_id)
                .context("coverage match has no declared slot")?;
            obligation.matched_capabilities.insert(reference.clone());
        }
        Ok(())
    }

    pub fn is_continuation_of(&self, previous: &Self) -> bool {
        self.obligations.len() >= previous.obligations.len()
            && self
                .obligations
                .iter()
                .zip(&previous.obligations)
                .all(|(next, old)| {
                    next.slot == old.slot
                        && old
                            .matched_capabilities
                            .is_subset(&next.matched_capabilities)
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn coverage_roundtrip_retains_omitted_slots_and_positive_witnesses(
            statements in prop::collection::vec("[a-z]{1,24}", 1..30),
            mask in prop::collection::vec(any::<bool>(), 30)
        ) {
            let mut coverage = DiscoveryCoverage::default();
            coverage.slots_for_turn(&statements).unwrap();
            for (entry, matched) in coverage.obligations.iter_mut().zip(mask) {
                if matched { entry.matched_capabilities.insert(CapabilityRef { catalog: "matrix".into(), capability: "read".into() }); }
            }
            let previous = coverage.clone();
            let mut decoded: DiscoveryCoverage = serde_json::from_value(serde_json::to_value(&coverage).unwrap()).unwrap();
            let pending = decoded.slots_for_turn(&["resolve a new relation".into()]).unwrap();
            prop_assert!(decoded.is_continuation_of(&previous));
            for old in previous.obligations() {
                prop_assert_eq!(pending.contains(&old.slot), old.matched_capabilities.is_empty());
            }
            prop_assert_eq!(decoded.obligations()[..previous.obligations.len()].to_vec(), previous.obligations);
        }
    }

    #[test]
    fn rejects_slot_rewrite_or_omission() {
        let mut before = DiscoveryCoverage::default();
        before
            .slots_for_turn(&["Read selected records".into()])
            .unwrap();
        let mut after = DiscoveryCoverage::default();
        after.slots_for_turn(&["Read all records".into()]).unwrap();
        assert!(!after.is_continuation_of(&before));
        assert!(!DiscoveryCoverage::default().is_continuation_of(&before));
        let mut wire = serde_json::to_value(&before).unwrap();
        wire["obligations"][0]["slot"]["id"] = "s1".into();
        assert!(serde_json::from_value::<DiscoveryCoverage>(wire).is_err());
    }
}
