//! Lift wire [`PlasmCompArtifact`] into topo-ordered executable steps for the host runner.

use plasm_core::{PlasmBindGraph, PlasmCompArtifact, PlasmReturn, PlasmStepPayload, StepId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlasmCompLiftError {
    #[error(transparent)]
    Validation(#[from] plasm_core::plasm_monad::PlasmCompValidationError),
    #[error("bind graph references missing step `{step}`")]
    MissingStep { step: String },
    #[error("step `{step}` is not present in the bind graph")]
    OrphanStep { step: String },
}

/// In-memory executable comp: bind graph + topo-ordered typed step payloads.
#[derive(Debug, Clone)]
pub(crate) struct ExecutablePlasmComp {
    pub bind: PlasmBindGraph,
    pub steps_topo: Vec<(StepId, PlasmStepPayload)>,
    pub return_: PlasmReturn,
    pub approval_gates: Vec<StepId>,
}

/// Validate comp wiring and materialize steps in bind topological order.
pub(crate) fn lift_executable_comp(
    artifact: &PlasmCompArtifact,
) -> Result<ExecutablePlasmComp, PlasmCompLiftError> {
    artifact.comp.validate()?;
    let mut steps_topo = Vec::with_capacity(artifact.comp.bind.topo.len());
    for id in &artifact.comp.bind.topo {
        let payload = artifact
            .comp
            .steps
            .get(id.as_str())
            .ok_or_else(|| PlasmCompLiftError::MissingStep {
                step: id.to_string(),
            })?
            .clone();
        steps_topo.push((id.clone(), payload));
    }
    if steps_topo.len() != artifact.comp.steps.len() {
        let topo_ids: std::collections::BTreeSet<_> =
            artifact.comp.bind.topo.iter().map(|s| s.as_str()).collect();
        for key in artifact.comp.steps.keys() {
            if !topo_ids.contains(key.as_str()) {
                return Err(PlasmCompLiftError::OrphanStep { step: key.clone() });
            }
        }
    }
    Ok(ExecutablePlasmComp {
        bind: artifact.comp.bind.clone(),
        steps_topo,
        return_: artifact.comp.return_.clone(),
        approval_gates: artifact.approval_gates.clone(),
    })
}
