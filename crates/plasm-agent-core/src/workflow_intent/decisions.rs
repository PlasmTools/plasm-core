//! Requirement-owned uncertainty, with host identities and dependency impact.
use super::{Interpretation, Requirement};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionDraft {
    pub question: String,
    pub alternatives: Vec<String>,
}
impl DecisionDraft {
    pub fn validate(&self) -> Result<()> {
        let unique: BTreeSet<_> = self.alternatives.iter().map(|s| s.trim()).collect();
        ensure!(
            !self.question.trim().is_empty()
                && (2..=8).contains(&self.alternatives.len())
                && unique.len() == self.alternatives.len()
                && !unique.contains(""),
            "invalid unresolved decision"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedDecision {
    pub id: String,
    pub question: DecisionDraft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionImpact {
    pub decision: UnresolvedDecision,
    pub requirement_ids: Vec<String>,
}

pub fn validate_requirement(r: &Requirement) -> Result<()> {
    if let Some(d) = &r.decision {
        d.question.validate()?;
        ensure!(
            r.id.strip_prefix('r')
                .is_some_and(|n| d.id == format!("d{n}")),
            "invalid decision identity"
        );
    }
    Ok(())
}

/// Forward closure is conservative: constraints, branches and ordering all
/// make downstream operations dependent on the owner's unresolved meaning.
/// Independent prohibitions stay independent; settled() still blocks globally.
pub fn impacts(i: &Interpretation) -> Vec<DecisionImpact> {
    let mut result = Vec::new();
    for r in &i.requirements {
        let Some(d) = &r.decision else { continue };
        let mut affected = BTreeSet::from([r.id.clone()]);
        loop {
            let before = affected.len();
            for edge in i.links.iter().flatten() {
                let (source, target) = edge.endpoints();
                if affected.contains(source) {
                    affected.insert(target.to_owned());
                }
            }
            if affected.len() == before {
                break;
            }
        }
        result.push(DecisionImpact {
            decision: d.clone(),
            requirement_ids: affected.into_iter().collect(),
        });
    }
    result.sort_by(|a, b| a.decision.id.cmp(&b.decision.id));
    result
}
