//! Host-owned conditional membership. Branch edges are derived, never selected.
use super::{
    links::RequirementLink, Interpretation, Requirement, RequirementDisposition, RequirementKind,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_DEPTH: usize = 4;

/// Internal lowering positions, before the host allocates requirement IDs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub predicate: usize,
    pub when_true: Vec<usize>,
    pub when_false: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ownership {
    pub predicate: String,
    pub when_true: Vec<String>,
    pub when_false: Vec<String>,
}
impl Ownership {
    fn members(&self) -> impl Iterator<Item = &String> {
        self.when_true.iter().chain(&self.when_false)
    }
    fn subtree(&self) -> BTreeSet<String> {
        self.members()
            .cloned()
            .chain(std::iter::once(self.predicate.clone()))
            .collect()
    }
}
fn operation(r: &Requirement) -> bool {
    matches!(
        r.kind,
        RequirementKind::Effect | RequirementKind::InformationNeed
    )
}

pub fn allocate(drafts: &[Draft], fresh: &[Requirement]) -> Result<Vec<Ownership>> {
    let id = |position: usize| -> Result<String> {
        Ok(fresh
            .get(position)
            .context("conditional position outside fresh requirements")?
            .id
            .clone())
    };
    drafts
        .iter()
        .map(|d| {
            Ok(Ownership {
                predicate: id(d.predicate)?,
                when_true: d.when_true.iter().map(|p| id(*p)).collect::<Result<_>>()?,
                when_false: d.when_false.iter().map(|p| id(*p)).collect::<Result<_>>()?,
            })
        })
        .collect()
}

pub fn validate(requirements: &[Requirement], owners: &[Ownership]) -> Result<()> {
    let rows: BTreeMap<_, _> = requirements.iter().map(|r| (r.id.as_str(), r)).collect();
    let expected: BTreeSet<_> = requirements
        .iter()
        .filter(|r| r.kind == RequirementKind::Condition)
        .map(|r| r.id.as_str())
        .collect();
    let actual: BTreeSet<_> = owners.iter().map(|o| o.predicate.as_str()).collect();
    ensure!(
        actual.len() == owners.len() && actual == expected,
        "every condition needs exactly one ownership manifest"
    );
    for owner in owners {
        let mut seen = BTreeSet::new();
        let mut operations = 0;
        for member in owner.members() {
            ensure!(
                member != &owner.predicate && seen.insert(member),
                "self or duplicate conditional member"
            );
            let row = rows
                .get(member.as_str())
                .context("unknown conditional member")?;
            ensure!(
                row.kind != RequirementKind::Prohibition,
                "prohibition cannot be a branch member"
            );
            operations += usize::from(operation(row));
        }
        ensure!(
            operations > 0,
            "conditional requires an affirmative operation"
        );
        let depth = 1 + owners
            .iter()
            .filter(|parent| parent.members().any(|id| id == &owner.predicate))
            .count();
        ensure!(
            depth <= MAX_DEPTH,
            "conditional nesting exceeds depth limit"
        );
        for child in owners
            .iter()
            .filter(|child| child.predicate != owner.predicate)
        {
            let branch = if owner.when_true.contains(&child.predicate) {
                Some(&owner.when_true)
            } else if owner.when_false.contains(&child.predicate) {
                Some(&owner.when_false)
            } else {
                None
            };
            if let Some(branch) = branch {
                ensure!(
                    child.members().all(|id| branch.contains(id)),
                    "nested conditional escapes its parent branch"
                );
            } else if !child.members().any(|id| id == &owner.predicate) {
                ensure!(
                    owner.subtree().is_disjoint(&child.subtree()),
                    "conditional subtrees overlap without ownership"
                );
            }
        }
    }
    for constraint in requirements
        .iter()
        .filter(|r| r.kind == RequirementKind::SelectionConstraint)
    {
        let scopes: Vec<_> = owners
            .iter()
            .flat_map(|o| [&o.when_true, &o.when_false])
            .filter(|branch| branch.contains(&constraint.id))
            .collect();
        ensure!(
            scopes.is_empty()
                || requirements.iter().any(|target| operation(target)
                    && scopes.iter().all(|branch| branch.contains(&target.id))),
            "branch constraint has no legal operation target"
        );
    }
    ensure!(
        edges(requirements, owners).len() <= 256,
        "too many fixed conditional edges"
    );
    Ok(())
}

pub fn edges(requirements: &[Requirement], owners: &[Ownership]) -> Vec<RequirementLink> {
    let operations: BTreeSet<_> = requirements
        .iter()
        .filter(|r| operation(r))
        .map(|r| &r.id)
        .collect();
    let mut edges = Vec::new();
    for owner in owners {
        for target in owner.when_true.iter().filter(|id| operations.contains(id)) {
            edges.push(RequirementLink::WhenTrue {
                source: owner.predicate.clone(),
                target: target.clone(),
            });
        }
        for target in owner.when_false.iter().filter(|id| operations.contains(id)) {
            edges.push(RequirementLink::WhenFalse {
                source: owner.predicate.clone(),
                target: target.clone(),
            });
        }
    }
    edges
}

/// Outermost conditional subtrees are indivisible revision units. Loose rows
/// remain individual units, using their existing host-issued requirement IDs.
pub fn units(i: &Interpretation) -> BTreeMap<String, BTreeSet<String>> {
    let mut units = BTreeMap::new();
    let members: BTreeSet<_> = i
        .conditionals
        .iter()
        .flat_map(|o| o.members().cloned())
        .collect();
    for owner in &i.conditionals {
        if !members.contains(&owner.predicate) {
            units.insert(owner.predicate.clone(), owner.subtree());
        }
    }
    let covered: BTreeSet<_> = units.values().flatten().cloned().collect();
    for row in &i.requirements {
        if !covered.contains(&row.id) {
            units.insert(row.id.clone(), BTreeSet::from([row.id.clone()]));
        }
    }
    units
}

pub fn expand_dispositions(
    i: Option<&Interpretation>,
    proposed: BTreeMap<String, RequirementDisposition>,
) -> Result<BTreeMap<String, RequirementDisposition>> {
    let units = i.map(units).unwrap_or_default();
    ensure!(
        units.keys().collect::<BTreeSet<_>>() == proposed.keys().collect::<BTreeSet<_>>(),
        "every revision unit needs exactly one disposition"
    );
    Ok(proposed
        .into_iter()
        .flat_map(|(id, d)| {
            units[&id]
                .iter()
                .cloned()
                .map(move |member| (member, d.clone()))
        })
        .collect())
}

pub fn retain(i: &Interpretation, retained: &BTreeSet<String>) -> Result<Vec<Ownership>> {
    for members in units(i).values() {
        let n = members.intersection(retained).count();
        ensure!(
            n == 0 || n == members.len(),
            "conditional revision unit must be retained or retired atomically"
        );
    }
    Ok(i.conditionals
        .iter()
        .filter(|o| retained.contains(&o.predicate))
        .cloned()
        .collect())
}

pub fn constraint_target_allowed(i: &Interpretation, source: &str, target: &str) -> bool {
    i.conditionals.iter().all(|o| {
        [&o.when_true, &o.when_false].into_iter().all(|branch| {
            !branch.iter().any(|id| id == source) || branch.iter().any(|id| id == target)
        })
    })
}
