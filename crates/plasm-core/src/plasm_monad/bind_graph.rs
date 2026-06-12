use super::comp::StepId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    pub fn validate(&self, known: &BTreeSet<String>) -> Result<(), String> {
        for id in &self.topo {
            if !known.contains(id.as_str()) {
                return Err(format!("bind.topo unknown step {id}"));
            }
        }
        for (step, deps) in &self.deps {
            if !known.contains(step.as_str()) {
                return Err(format!("bind.deps unknown step {step}"));
            }
            for d in deps {
                if !known.contains(d.as_str()) {
                    return Err(format!("bind.deps[{step}] references unknown {d}"));
                }
            }
        }
        Ok(())
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
