//! Relations are resolved only after the host has assigned requirement IDs.
use super::{Interpretation, RequirementKind};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequirementLink {
    Constrains { source: String, target: String },
    WhenTrue { source: String, target: String },
    WhenFalse { source: String, target: String },
    Before { source: String, target: String },
}
impl RequirementLink {
    pub fn endpoints(&self) -> (&str, &str) {
        match self {
            Self::Constrains { source, target }
            | Self::WhenTrue { source, target }
            | Self::WhenFalse { source, target }
            | Self::Before { source, target } => (source, target),
        }
    }
}
fn operation(kind: &RequirementKind) -> bool {
    matches!(
        kind,
        RequirementKind::Effect | RequirementKind::InformationNeed
    )
}
/// Request-local identifiers supplied by the host. The model selects whole
/// well-typed edges, never independently generated source/target combinations.
#[derive(Debug, Clone, Serialize)]
pub struct LinkChoice {
    pub id: String,
    pub link: RequirementLink,
}
pub fn choices(interpretation: &Interpretation) -> Vec<LinkChoice> {
    let mut requirements: Vec<_> = interpretation.requirements.iter().collect();
    requirements.sort_by(|a, b| a.id.cmp(&b.id));
    let mut choices = Vec::new();
    for source in &requirements {
        for target in &requirements {
            if source.id == target.id || !operation(&target.kind) {
                continue;
            }
            let from = source.id.clone();
            let to = target.id.clone();
            let edges = match source.kind {
                RequirementKind::SelectionConstraint => vec![RequirementLink::Constrains {
                    source: from,
                    target: to,
                }],
                RequirementKind::Condition => vec![],
                RequirementKind::Effect | RequirementKind::InformationNeed => {
                    vec![RequirementLink::Before {
                        source: from,
                        target: to,
                    }]
                }
                RequirementKind::Prohibition => vec![],
            };
            for link in edges {
                if matches!(link, RequirementLink::Constrains { .. })
                    && !super::conditional::constraint_target_allowed(
                        interpretation,
                        &source.id,
                        &target.id,
                    )
                {
                    continue;
                }
                choices.push(LinkChoice {
                    id: format!("l{}", choices.len()),
                    link,
                });
            }
        }
    }
    choices
}

/// Every required attachment needs at least one legal target before asking the
/// model to choose. Optional ordering edges need not be chosen, so a witness
/// consisting of one attachment per qualifier cannot introduce order cycles.
pub fn ensure_feasible(interpretation: &Interpretation, choices: &[LinkChoice]) -> Result<()> {
    super::conditional::validate(&interpretation.requirements, &interpretation.conditionals)?;
    for requirement in &interpretation.requirements {
        if matches!(requirement.kind, RequirementKind::SelectionConstraint) {
            ensure!(
                choices
                    .iter()
                    .any(|choice| choice.link.endpoints().0 == requirement.id),
                "unlinkable interpretation: {} has no legal operation target",
                requirement.id
            );
        }
    }
    Ok(())
}

pub fn validate(interpretation: &Interpretation, links: &[RequirementLink]) -> Result<()> {
    ensure!(links.len() <= 256, "too many requirement links");
    super::conditional::validate(&interpretation.requirements, &interpretation.conditionals)?;
    let fixed =
        super::conditional::edges(&interpretation.requirements, &interpretation.conditionals);
    let actual: BTreeSet<_> = links
        .iter()
        .filter(|e| {
            matches!(
                e,
                RequirementLink::WhenTrue { .. } | RequirementLink::WhenFalse { .. }
            )
        })
        .collect();
    let expected: BTreeSet<_> = fixed.iter().collect();
    ensure!(
        actual == expected,
        "conditional edges must match structural ownership"
    );
    let requirements: BTreeMap<_, _> = interpretation
        .requirements
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let mut seen = BTreeSet::new();
    let mut attached = BTreeSet::new();
    let mut conditional_pairs = BTreeSet::new();
    let mut ordering: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for link in links {
        let (source, target) = link.endpoints();
        ensure!(source != target, "self-linked requirement");
        let from = requirements
            .get(source)
            .ok_or_else(|| anyhow::anyhow!("unknown link source"))?;
        let to = requirements
            .get(target)
            .ok_or_else(|| anyhow::anyhow!("unknown link target"))?;
        ensure!(
            seen.insert(serde_json::to_string(link)?),
            "duplicate requirement link"
        );
        ensure!(
            operation(&to.kind),
            "link target must be an effect or information need"
        );
        match link {
            RequirementLink::Constrains { .. } => {
                ensure!(
                    from.kind == RequirementKind::SelectionConstraint,
                    "constraint edge needs a selection constraint"
                );
                ensure!(
                    super::conditional::constraint_target_allowed(interpretation, source, target),
                    "constraint escapes its conditional body"
                );
                attached.insert(source);
            }
            RequirementLink::WhenTrue { .. } | RequirementLink::WhenFalse { .. } => {
                ensure!(
                    from.kind == RequirementKind::Condition,
                    "conditional edge needs a condition"
                );
                ensure!(
                    conditional_pairs.insert((source, target)),
                    "opposing conditional branches for one operation"
                );
                attached.insert(source);
            }
            RequirementLink::Before { .. } => {
                ensure!(
                    operation(&from.kind),
                    "ordering source must be an operation"
                );
                ordering.entry(source).or_default().push(target);
            }
        }
    }
    for r in &interpretation.requirements {
        if matches!(
            r.kind,
            RequirementKind::SelectionConstraint | RequirementKind::Condition
        ) {
            ensure!(
                attached.contains(r.id.as_str()),
                "unattached condition or selection constraint"
            );
        }
    }
    // At most 64 requirements: deterministic bounded traversal for each root.
    for root in requirements.keys() {
        let mut pending = ordering.get(root).cloned().unwrap_or_default();
        let mut visited = BTreeSet::new();
        while let Some(node) = pending.pop() {
            ensure!(&node != root, "cyclic requirement ordering");
            if visited.insert(node) {
                pending.extend(ordering.get(node).into_iter().flatten().copied());
            }
        }
    }
    Ok(())
}

pub fn queries(
    interpretation: &Interpretation,
    links: &[RequirementLink],
) -> Result<Vec<(String, String)>> {
    validate(interpretation, links)?;
    Ok(interpretation
        .requirements
        .iter()
        .map(|r| {
            let mut statements = vec![r.statement.clone()];
            for link in links {
                let (source, target) = link.endpoints();
                if target == r.id {
                    let other = interpretation
                        .requirements
                        .iter()
                        .find(|x| x.id == source)
                        .expect("validated link");
                    let relation = match link {
                        RequirementLink::Constrains { .. } => "Selection constraint",
                        RequirementLink::WhenTrue { .. } => "Only when",
                        RequirementLink::WhenFalse { .. } => "Only when false",
                        RequirementLink::Before { .. } => "After",
                    };
                    statements.push(format!("{relation}: {}", other.statement));
                } else if source == r.id && !matches!(link, RequirementLink::Before { .. }) {
                    let other = interpretation
                        .requirements
                        .iter()
                        .find(|x| x.id == target)
                        .expect("validated link");
                    statements.push(format!("Applies to: {}", other.statement));
                }
            }
            (r.id.clone(), statements.join("\n"))
        })
        .collect())
}
