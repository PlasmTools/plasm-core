//! Shared workflow interpretation contract. Evaluated offline before host cutover.
//!
//! User turns are authoritative input; inferred requirements are revisable model
//! judgments, never authorization. No catalog acquisition or mutation happens here.
pub mod conditional;
pub mod contract;
pub mod decisions;
pub mod links;
pub mod retrieval;
pub mod staged;

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn required_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentScope {
    Workflow,
    Focus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserTurn {
    pub id: String,
    pub request_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Effect,
    /// An independent restriction on allowed behavior, never an operation to execute.
    Prohibition,
    SelectionConstraint,
    Condition,
    InformationNeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementDraft {
    #[serde(deserialize_with = "required_option")]
    pub uncertainty: Option<decisions::DecisionDraft>,
    pub kind: RequirementKind,
    pub statement: String,
    pub source_turn_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    #[serde(deserialize_with = "required_option")]
    pub decision: Option<decisions::UnresolvedDecision>,
    pub id: String,
    pub kind: RequirementKind,
    pub statement: String,
    pub source_turn_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterpretationDraft {
    /// Derived only by the host-owned commit lowering, never decoded from provider JSON.
    #[serde(skip)]
    pub conditionals: Vec<conditional::Draft>,
    #[serde(deserialize_with = "deserialize_dispositions")]
    pub dispositions: BTreeMap<String, RequirementDisposition>,
    pub requirements: Vec<RequirementDraft>,
    /// Required when there are no requirements (e.g. a conversational-only turn).
    pub no_requirements_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequirementDisposition {
    Retain {},
    Retire {
        reason: String,
        source_turn_ids: Vec<String>,
    },
}

fn deserialize_dispositions<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, RequirementDisposition>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, RequirementDisposition>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("one disposition per requirement ID")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((id, disposition)) =
                map.next_entry::<String, RequirementDisposition>()?
            {
                if values.insert(id, disposition).is_some() {
                    return Err(serde::de::Error::custom("duplicate disposition key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Unique)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredRequirement {
    pub requirement: Requirement,
    pub reason: String,
    pub source_turn_ids: Vec<String>,
    pub intent_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interpretation {
    pub conditionals: Vec<conditional::Ownership>,
    pub intent_revision: u64,
    pub version: u64,
    pub requirements: Vec<Requirement>,
    pub retired: Vec<RetiredRequirement>,
    pub links: Option<Vec<links::RequirementLink>>,
    pub no_requirements_reason: Option<String>,
}

/// Serializable state for host-owned workflow storage. Fields are private so all
/// updates cross the same checked interface. Deserialized state must be validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowIntent {
    scope: IntentScope,
    revision: u64,
    turns: Vec<UserTurn>,
    interpretation: Option<Interpretation>,
    next_requirement: u64,
}

impl WorkflowIntent {
    pub fn open(scope: IntentScope, request_id: String, text: String) -> Result<Self> {
        ensure!(
            !request_id.trim().is_empty() && !text.trim().is_empty(),
            "empty intent input"
        );
        Ok(Self {
            scope,
            revision: 1,
            turns: vec![UserTurn {
                id: "u0".into(),
                request_id,
                text,
            }],
            interpretation: None,
            next_requirement: 0,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn turns(&self) -> &[UserTurn] {
        &self.turns
    }
    pub fn interpretation(&self) -> Option<&Interpretation> {
        self.interpretation.as_ref()
    }

    /// Retries of identical user submissions are idempotent. A reused request ID
    /// with different content is an error even when the revision is current.
    pub fn append(
        &mut self,
        expected_revision: u64,
        request_id: String,
        text: String,
    ) -> Result<u64> {
        self.validate()?;
        ensure!(
            !request_id.trim().is_empty() && !text.trim().is_empty(),
            "empty intent input"
        );
        if let Some(prior) = self.turns.iter().find(|t| t.request_id == request_id) {
            ensure!(
                prior.text == text,
                "request ID reused with different intent"
            );
            return Ok(self.revision);
        }
        ensure!(expected_revision == self.revision, "stale intent revision");
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("intent revision overflow"))?;
        self.turns.push(UserTurn {
            id: format!("u{}", self.turns.len()),
            request_id,
            text,
        });
        self.revision = revision;
        // Retain the prior interpretation for explicit reconciliation, but its
        // revision can no longer be used for retrieval or assessment.
        Ok(revision)
    }

    /// Interpret turns in order, even when several were queued before discovery.
    pub fn next_interpretation_revision(&self) -> u64 {
        self.interpretation.as_ref().map_or(1, |i| {
            i.intent_revision.saturating_add(1).min(self.revision)
        })
    }

    pub fn current(&self) -> Result<&Interpretation> {
        self.validate()?;
        let value = self
            .interpretation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("intent not interpreted"))?;
        ensure!(
            value.intent_revision == self.revision,
            "interpretation is stale"
        );
        Ok(value)
    }

    /// Compare both revisions: two concurrent interpretations of the same user
    /// turns must not silently overwrite each other. Storage must CAS this state.
    pub fn interpret(
        &mut self,
        expected_revision: u64,
        expected_version: u64,
        draft: InterpretationDraft,
    ) -> Result<&Interpretation> {
        self.validate()?;
        ensure!(expected_revision == self.revision, "stale intent revision");
        let prior_version = self.interpretation.as_ref().map_or(0, |v| v.version);
        ensure!(
            expected_version == prior_version,
            "stale interpretation version"
        );
        let retained_ids: Vec<_> = draft
            .dispositions
            .iter()
            .filter_map(|(id, d)| {
                matches!(d, RequirementDisposition::Retain {}).then_some(id.clone())
            })
            .collect();
        ensure!(
            draft.requirements.len() + retained_ids.len() <= 64,
            "too many intent requirements"
        );
        ensure!(
            (draft.requirements.is_empty() && retained_ids.is_empty())
                == draft.no_requirements_reason.is_some(),
            "empty interpretation needs an explicit reason only"
        );
        if let Some(reason) = &draft.no_requirements_reason {
            ensure!(!reason.trim().is_empty(), "blank no-requirements reason");
        }
        let target_revision = self.next_interpretation_revision();
        let mut dispositions = BTreeSet::new();
        let mut retired = self
            .interpretation
            .as_ref()
            .map(|i| i.retired.clone())
            .unwrap_or_default();
        for (requirement_id, decision) in &draft.dispositions {
            let RequirementDisposition::Retire {
                reason,
                source_turn_ids,
            } = decision
            else {
                continue;
            };
            ensure!(
                dispositions.insert(requirement_id.clone()),
                "duplicate retirement"
            );
            let prior = self
                .interpretation
                .as_ref()
                .and_then(|i| i.requirements.iter().find(|r| r.id == *requirement_id))
                .ok_or_else(|| anyhow::anyhow!("unknown retired requirement"))?;
            ensure!(!reason.trim().is_empty(), "retirement needs a reason");
            let sources: BTreeSet<_> = source_turn_ids.iter().collect();
            ensure!(
                sources.len() == source_turn_ids.len()
                    && source_turn_ids.contains(&format!("u{}", target_revision - 1))
                    && sources.iter().all(|id| self
                        .turns
                        .iter()
                        .take(target_revision as usize)
                        .any(|t| &t.id == *id)),
                "retirement must cite the current user revision and known sources"
            );
            retired.push(RetiredRequirement {
                requirement: prior.clone(),
                reason: reason.clone(),
                source_turn_ids: source_turn_ids.clone(),
                intent_revision: target_revision,
            });
        }
        for id in &retained_ids {
            ensure!(
                dispositions.insert(id.clone()),
                "requirement cannot be retained and retired"
            );
        }
        let expected: BTreeSet<_> = self
            .interpretation
            .as_ref()
            .map(|i| i.requirements.iter().map(|r| r.id.clone()).collect())
            .unwrap_or_default();
        ensure!(
            dispositions == expected,
            "every prior requirement needs an explicit retain or retire disposition"
        );
        let mut used = BTreeSet::new();
        let mut next = self.next_requirement;
        let mut requirements = Vec::new();
        let retained_set: BTreeSet<_> = retained_ids.iter().cloned().collect();
        let mut conditionals = self
            .interpretation
            .as_ref()
            .map(|i| conditional::retain(i, &retained_set))
            .transpose()?
            .unwrap_or_default();
        let retained_count = retained_ids.len();
        for id in retained_ids {
            let prior = self
                .interpretation
                .as_ref()
                .and_then(|v| v.requirements.iter().find(|r| r.id == id))
                .ok_or_else(|| anyhow::anyhow!("unknown retained requirement"))?;
            ensure!(used.insert(id), "duplicate retained requirement");
            requirements.push(prior.clone());
        }
        for item in draft.requirements {
            ensure!(!item.statement.trim().is_empty(), "blank requirement");
            ensure!(
                !item.source_turn_ids.is_empty(),
                "requirement has no source turn"
            );
            let sources: BTreeSet<_> = item.source_turn_ids.iter().collect();
            ensure!(
                sources.len() == item.source_turn_ids.len()
                    && sources.iter().all(|id| self
                        .turns
                        .iter()
                        .take(target_revision as usize)
                        .any(|t| &t.id == *id)),
                "unknown or duplicate source turn"
            );
            let id = format!("r{next}");
            next = next
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("requirement identity overflow"))?;
            ensure!(used.insert(id.clone()), "duplicate requirement identity");
            let decision = item
                .uncertainty
                .map(|question| {
                    question.validate()?;
                    Ok::<_, anyhow::Error>(decisions::UnresolvedDecision {
                        id: format!("d{}", next - 1),
                        question,
                    })
                })
                .transpose()?;
            requirements.push(Requirement {
                decision,
                id,
                kind: item.kind,
                statement: item.statement,
                source_turn_ids: item.source_turn_ids,
            });
        }
        conditionals.extend(conditional::allocate(
            &draft.conditionals,
            &requirements[retained_count..],
        )?);
        conditional::validate(&requirements, &conditionals)?;
        let version = prior_version
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("interpretation version overflow"))?;
        self.interpretation = Some(Interpretation {
            conditionals,
            intent_revision: target_revision,
            version,
            requirements,
            retired,
            links: None,
            no_requirements_reason: draft.no_requirements_reason,
        });
        self.next_requirement = next;
        // Prefix interpretations are valid state but cannot enter retrieval.
        Ok(self
            .interpretation
            .as_ref()
            .expect("interpretation just assigned"))
    }

    pub fn set_links(
        &mut self,
        expected_revision: u64,
        expected_version: u64,
        links: Vec<links::RequirementLink>,
    ) -> Result<()> {
        let current = self.current()?;
        ensure!(
            expected_revision == self.revision && expected_version == current.version,
            "stale link interpretation"
        );
        ensure!(current.links.is_none(), "interpretation already linked");
        links::validate(current, &links)?;
        self.interpretation.as_mut().expect("current checked").links = Some(links);
        Ok(())
    }

    pub fn linked(&self) -> Result<&Interpretation> {
        let current = self.current()?;
        ensure!(
            current.links.is_some(),
            "requirement relationships not resolved"
        );
        Ok(current)
    }

    /// Capability availability cannot settle an unresolved interpretation.
    pub fn settled(&self) -> Result<&Interpretation> {
        let current = self.linked()?;
        ensure!(
            current.requirements.iter().all(|r| r.decision.is_none()),
            "workflow has unresolved interpretation decisions"
        );
        Ok(current)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.turns.is_empty() && self.revision == self.turns.len() as u64,
            "invalid intent revision"
        );
        let mut requests = BTreeSet::new();
        for (n, t) in self.turns.iter().enumerate() {
            ensure!(
                t.id == format!("u{n}")
                    && !t.request_id.trim().is_empty()
                    && !t.text.trim().is_empty()
                    && requests.insert(&t.request_id),
                "invalid user turn"
            );
        }
        if let Some(value) = &self.interpretation {
            conditional::validate(&value.requirements, &value.conditionals)?;
            if let Some(links) = &value.links {
                links::validate(value, links)?;
            }
            ensure!(
                value.requirements.len() <= 64,
                "too many restored requirements"
            );
            ensure!(
                value
                    .no_requirements_reason
                    .as_ref()
                    .is_none_or(|r| !r.trim().is_empty()),
                "blank restored empty reason"
            );
            let mut retired_ids = BTreeSet::new();
            for retired in &value.retired {
                let r = &retired.requirement;
                decisions::validate_requirement(r)?;
                let n = r.id.strip_prefix('r').and_then(|n| n.parse::<u64>().ok());
                let sources: BTreeSet<_> = retired.source_turn_ids.iter().collect();
                ensure!(
                    n.is_some_and(|n| n < self.next_requirement && r.id == format!("r{n}"))
                        && retired_ids.insert(&r.id)
                        && !value.requirements.iter().any(|a| a.id == r.id)
                        && !retired.reason.trim().is_empty()
                        && !r.statement.trim().is_empty()
                        && retired.intent_revision > 0
                        && retired.intent_revision <= value.intent_revision
                        && sources.len() == retired.source_turn_ids.len()
                        && retired
                            .source_turn_ids
                            .contains(&format!("u{}", retired.intent_revision - 1))
                        && sources.iter().all(|id| self
                            .turns
                            .iter()
                            .take(retired.intent_revision as usize)
                            .any(|t| &t.id == *id))
                        && !r.source_turn_ids.is_empty()
                        && r.source_turn_ids.iter().collect::<BTreeSet<_>>().len()
                            == r.source_turn_ids.len()
                        && r.source_turn_ids.iter().all(|id| self
                            .turns
                            .iter()
                            .take(retired.intent_revision as usize)
                            .any(|t| &t.id == id)),
                    "invalid retired requirement history"
                );
            }
            ensure!(
                value.intent_revision > 0
                    && value.intent_revision <= self.revision
                    && value.version >= value.intent_revision,
                "invalid interpretation revision"
            );
            let mut ids = BTreeSet::new();
            for r in &value.requirements {
                decisions::validate_requirement(r)?;
                let n = r.id.strip_prefix('r').and_then(|n| n.parse::<u64>().ok());
                ensure!(
                    n.is_some_and(|n| n < self.next_requirement && r.id == format!("r{n}"))
                        && ids.insert(&r.id),
                    "invalid requirement identity"
                );
                ensure!(
                    !r.statement.trim().is_empty() && !r.source_turn_ids.is_empty(),
                    "invalid requirement"
                );
                let sources: BTreeSet<_> = r.source_turn_ids.iter().collect();
                ensure!(
                    sources.len() == r.source_turn_ids.len()
                        && sources.iter().all(|id| self
                            .turns
                            .iter()
                            .take(value.intent_revision as usize)
                            .any(|t| &t.id == *id)),
                    "invalid source turn"
                );
            }
            ensure!(
                value.requirements.len() as u64 + value.retired.len() as u64
                    == self.next_requirement,
                "allocated requirement missing from active or retired history"
            );
            ensure!(
                value.requirements.is_empty() == value.no_requirements_reason.is_some(),
                "invalid empty interpretation"
            );
        } else if self.next_requirement != 0 {
            bail!("requirement allocator without interpretation");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
