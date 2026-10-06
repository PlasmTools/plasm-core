//! Parse the normative prose's evidence blocks; inventory is not execution proof.
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum Role {
    PositiveLive,
    NegativeAdmission,
    RuntimeEvidence,
    Metamorphic,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Evidence {
    Matrix {
        id: String,
    },
    Property {
        id: super::conformance_properties::Property,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Check {
    role: Role,
    claim: String,
    evidence: Vec<Evidence>,
    gap: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Law {
    pub id: String,
    pub extends: Vec<String>,
    checks: Vec<Check>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Status {
    Open,
    Partial,
    Discharged,
}

impl Law {
    pub fn status(&self) -> Status {
        if self.checks.iter().all(|check| check.evidence.is_empty()) {
            Status::Open
        } else if self.checks.iter().any(|check| check.gap.is_some()) {
            Status::Partial
        } else {
            Status::Discharged
        }
    }
}

#[derive(Debug)]
pub(super) enum ContractError {
    UnterminatedBlock,
    Json(serde_json::Error),
    Heading {
        law: String,
    },
    DuplicateLaw {
        law: String,
    },
    Inventory {
        prefix: String,
        count: usize,
    },
    OriginalLinks {
        law: String,
    },
    EvidenceRoles {
        law: String,
    },
    Obligation {
        law: String,
        role: Role,
    },
    UnknownWitness {
        id: String,
    },
    WitnessRole {
        id: String,
        role: Role,
    },
    UnregisteredProperty {
        property: super::conformance_properties::Property,
    },
    PropertyRole {
        property: super::conformance_properties::Property,
        role: Role,
    },
    DuplicateEvidence {
        law: String,
    },
}
impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "literate contract rejected: {self:?}")
    }
}
impl std::error::Error for ContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            _ => None,
        }
    }
}
pub(super) fn parse(document: &str) -> Result<BTreeMap<String, Law>, ContractError> {
    let mut laws = BTreeMap::new();
    let mut heading = "";
    let mut lines = document.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("### ") {
            heading = line;
        }
        if line != "```plasm-law" {
            continue;
        }
        let mut body = String::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line == "```" {
                closed = true;
                break;
            }
            body.push_str(line);
            body.push('\n');
        }
        if !closed {
            return Err(ContractError::UnterminatedBlock);
        }
        let law: Law = serde_json::from_str(&body).map_err(ContractError::Json)?;
        if !heading.starts_with(&format!("### {} — ", law.id)) {
            return Err(ContractError::Heading { law: law.id });
        }
        let id = law.id.clone();
        if laws.insert(id.clone(), law).is_some() {
            return Err(ContractError::DuplicateLaw { law: id });
        }
    }
    Ok(laws)
}

pub(super) fn validate(
    laws: &BTreeMap<String, Law>,
    prefix: &str,
    count: usize,
) -> Result<(), ContractError> {
    let required: BTreeSet<_> = (1..=count).map(|n| format!("{prefix}-{n:02}")).collect();
    if laws.keys().cloned().collect::<BTreeSet<_>>() != required {
        return Err(ContractError::Inventory {
            prefix: prefix.into(),
            count,
        });
    }
    let original: BTreeSet<_> = super::all_rows().map(|row| row.id).collect();
    let cases: BTreeMap<_, _> = super::python::cases().map(|case| (case.id, case)).collect();
    let roles = BTreeSet::from([
        Role::PositiveLive,
        Role::NegativeAdmission,
        Role::RuntimeEvidence,
        Role::Metamorphic,
    ]);
    for law in laws.values() {
        let links: BTreeSet<_> = law.extends.iter().map(String::as_str).collect();
        if links.is_empty() || links.len() != law.extends.len() || !links.is_subset(&original) {
            return Err(ContractError::OriginalLinks {
                law: law.id.clone(),
            });
        }
        if law.checks.iter().map(|c| c.role).collect::<BTreeSet<_>>() != roles {
            return Err(ContractError::EvidenceRoles {
                law: law.id.clone(),
            });
        }
        for check in &law.checks {
            if check.claim.trim().is_empty()
                || check.gap.as_ref().is_some_and(|gap| gap.trim().is_empty())
                || (check.evidence.is_empty() && check.gap.is_none())
            {
                return Err(ContractError::Obligation {
                    law: law.id.clone(),
                    role: check.role,
                });
            }
            let mut unique = BTreeSet::new();
            for evidence in &check.evidence {
                let key = match evidence {
                    Evidence::Matrix { id } => {
                        let case = cases
                            .get(id.as_str())
                            .ok_or_else(|| ContractError::UnknownWitness { id: id.clone() })?;
                        let outcome = super::python_render_parity::python_outcome(case.id);
                        let admissible = match check.role {
                            Role::PositiveLive => {
                                case.expect_live_error.is_none()
                                    && !matches!(
                                        outcome,
                                        super::python::PythonOutcome::CompileError(_)
                                            | super::python::PythonOutcome::LiveError(_)
                                    )
                            }
                            Role::NegativeAdmission => {
                                matches!(outcome, super::python::PythonOutcome::CompileError(_))
                            }
                            Role::RuntimeEvidence => {
                                !matches!(outcome, super::python::PythonOutcome::CompileError(_))
                            }
                            Role::Metamorphic => false,
                        };
                        if !admissible {
                            return Err(ContractError::WitnessRole {
                                id: id.clone(),
                                role: check.role,
                            });
                        }
                        format!("matrix:{id}")
                    }
                    Evidence::Property { id } => {
                        if !super::conformance_properties::PROPERTIES.contains(id) {
                            return Err(ContractError::UnregisteredProperty { property: *id });
                        }
                        if check.role != id.role() {
                            return Err(ContractError::PropertyRole {
                                property: *id,
                                role: check.role,
                            });
                        }
                        format!("property:{id:?}")
                    }
                };
                if !unique.insert(key) {
                    return Err(ContractError::DuplicateEvidence {
                        law: law.id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

pub(super) fn report(laws: &BTreeMap<String, Law>, label: &str) -> String {
    let count = |status| laws.values().filter(|law| law.status() == status).count();
    let checks = laws.values().flat_map(|law| &law.checks).count();
    let gaps = laws
        .values()
        .flat_map(|law| &law.checks)
        .filter(|c| c.gap.is_some())
        .count();
    format!("{label}: {} laws; {} open, {} partial, {} finite obligations discharged; {gaps}/{checks} checks retain gaps. Registration is not an execution result or a proof over arbitrary programs.", laws.len(), count(Status::Open), count(Status::Partial), count(Status::Discharged))
}

#[test]
fn malformed_literate_contracts_fail_closed() {
    let error = parse("```plasm-law\n{\n```").unwrap_err();
    assert!(matches!(error, ContractError::Json(_)));
    assert!(std::error::Error::source(&error)
        .unwrap()
        .is::<serde_json::Error>());
    assert!(matches!(
        parse("```plasm-law\n{}"),
        Err(ContractError::UnterminatedBlock)
    ));
    assert!(
        matches!(parse("```plasm-law\n{\"id\":\"X\",\"extends\":[],\"checks\":[]}\n```"), Err(ContractError::Heading { law }) if law == "X")
    );
}
