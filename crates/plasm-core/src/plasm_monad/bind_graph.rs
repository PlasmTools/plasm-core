use super::comp::StepId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BindGraphError {
    #[error("bind.topo references unknown step `{step}`")]
    UnknownTopologicalStep { step: String },
    #[error("bind.deps references unknown step `{step}`")]
    UnknownDependencyOwner { step: String },
    #[error("bind.deps[{step}] references unknown dependency `{dependency}`")]
    UnknownDependency { step: String, dependency: String },
    #[error("bind.topo contains duplicate steps")]
    DuplicateTopologicalStep,
    #[error("bind graph contains an empty step id")]
    EmptyStepId,
    #[error("bind body shadows captured input `{step}`")]
    CapturedInputShadowed { step: String },
    #[error("bind.deps[{step}] escapes the body scope")]
    DependencyEscapesBodyScope { step: String },
    #[error("bind.primary[{step}] is not a declared dependency")]
    PrimaryIsNotDependency { step: String },
    #[error("bind.holes references unknown step `{step}`")]
    UnknownHoleOwner { step: String },
    #[error(
        "bind.holes[{step}] has a duplicate/empty alias or undeclared dependency `{dependency}`"
    )]
    InvalidHole { step: String, dependency: String },
    #[error("plan bind graph has cyclic or unsatisfiable step dependencies")]
    CyclicOrUnsatisfiableDependencies,
}

/// Monadic bind witness: execution order + dependency closure for a Plasm program.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlasmBindGraph {
    pub topo: Vec<StepId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deps: BTreeMap<StepId, BTreeSet<StepId>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub primary: BTreeMap<StepId, StepId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holes: BTreeMap<StepId, Vec<PlasmHoleUse>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlasmHoleUse {
    pub step: StepId,
    pub alias: String,
}

impl PlasmBindGraph {
    pub fn validate(&self, known: &BTreeSet<String>) -> Result<(), BindGraphError> {
        for id in &self.topo {
            if !known.contains(id.as_str()) {
                return Err(BindGraphError::UnknownTopologicalStep {
                    step: id.as_str().to_owned(),
                });
            }
        }
        for (step, deps) in &self.deps {
            if !known.contains(step.as_str()) {
                return Err(BindGraphError::UnknownDependencyOwner {
                    step: step.as_str().to_owned(),
                });
            }
            for d in deps {
                if !known.contains(d.as_str()) {
                    return Err(BindGraphError::UnknownDependency {
                        step: step.as_str().to_owned(),
                        dependency: d.as_str().to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Schedule the existing bind graph with explicitly supplied outer inputs.
    /// Inputs are already materialized and are never scheduled as body steps.
    /// The ordinary program scheduler calls this with an empty input set.
    pub fn execution_layers(
        &self,
        inputs: &BTreeSet<StepId>,
    ) -> Result<Vec<Vec<StepId>>, BindGraphError> {
        let scheduled: BTreeSet<_> = self.topo.iter().cloned().collect();
        if scheduled.len() != self.topo.len() {
            return Err(BindGraphError::DuplicateTopologicalStep);
        }
        if scheduled
            .iter()
            .chain(inputs)
            .any(|id| id.as_str().trim().is_empty())
        {
            return Err(BindGraphError::EmptyStepId);
        }
        if let Some(step) = scheduled.intersection(inputs).next() {
            return Err(BindGraphError::CapturedInputShadowed {
                step: step.as_str().to_owned(),
            });
        }
        let known: BTreeSet<_> = scheduled.union(inputs).cloned().collect();
        for (step, deps) in &self.deps {
            if !scheduled.contains(step) || !deps.is_subset(&known) {
                return Err(BindGraphError::DependencyEscapesBodyScope {
                    step: step.as_str().to_owned(),
                });
            }
        }
        for (step, source) in &self.primary {
            if !scheduled.contains(step)
                || !self
                    .deps
                    .get(step)
                    .is_some_and(|deps| deps.contains(source))
            {
                return Err(BindGraphError::PrimaryIsNotDependency {
                    step: step.as_str().to_owned(),
                });
            }
        }
        for (step, holes) in &self.holes {
            if !scheduled.contains(step) {
                return Err(BindGraphError::UnknownHoleOwner {
                    step: step.as_str().to_owned(),
                });
            }
            let mut aliases = BTreeSet::new();
            for hole in holes {
                if hole.alias.trim().is_empty()
                    || !aliases.insert(&hole.alias)
                    || !self
                        .deps
                        .get(step)
                        .is_some_and(|deps| deps.contains(&hole.step))
                {
                    return Err(BindGraphError::InvalidHole {
                        step: step.as_str().to_owned(),
                        dependency: hole.step.as_str().to_owned(),
                    });
                }
            }
        }
        let mut remaining = scheduled;
        let mut done = inputs.clone();
        let mut layers = Vec::new();
        while !remaining.is_empty() {
            let layer: Vec<_> = self
                .topo
                .iter()
                .filter(|id| {
                    remaining.contains(*id)
                        && self.deps.get(*id).is_none_or(|deps| deps.is_subset(&done))
                })
                .cloned()
                .collect();
            if layer.is_empty() {
                return Err(BindGraphError::CyclicOrUnsatisfiableDependencies);
            }
            for id in &layer {
                remaining.remove(id);
                done.insert(id.clone());
            }
            layers.push(layer);
        }
        Ok(layers)
    }

    pub fn respects_effect_order(
        topo: &[StepId],
        write_barrier_after: &BTreeMap<StepId, usize>,
    ) -> bool {
        for (step, &idx) in write_barrier_after {
            for other in write_barrier_after.keys() {
                if other == step {
                    continue;
                }
                let other_idx = write_barrier_after[other];
                if other_idx < idx {
                    if let Some(pos) = topo.iter().position(|s| s == other) {
                        if let Some(pos_step) = topo.iter().position(|s| s == step) {
                            if pos > pos_step {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    }
}
