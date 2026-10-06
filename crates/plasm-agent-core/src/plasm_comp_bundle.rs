//! Runnable monadic comp: wire [`PlasmCompArtifact`] + lifted executable payload (in-memory only).

use crate::plasm_comp_lift::{lift_executable_comp, ExecutablePlasmComp};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlasmCompBundleError {
    #[error(transparent)]
    Lift(#[from] crate::plasm_comp_lift::PlasmCompLiftError),
}

impl From<PlasmCompBundleError> for plasm_runtime::ExecutionFailure {
    fn from(error: PlasmCompBundleError) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Program,
            "plasm_comp_bundle_invalid",
            error.to_string(),
        )
    }
}
use plasm_core::PlasmCompArtifact;

/// Canonical compile/run artifact. Wire consumers see [`PlasmCompArtifact::comp`] only.
#[derive(Debug, Clone)]
pub struct PlasmCompBundle {
    artifact: PlasmCompArtifact,
    executable: ExecutablePlasmComp,
}

impl PlasmCompBundle {
    pub fn new(artifact: PlasmCompArtifact) -> Result<Self, PlasmCompBundleError> {
        let executable = lift_executable_comp(&artifact)?;
        Ok(Self {
            artifact,
            executable,
        })
    }

    pub fn artifact(&self) -> &PlasmCompArtifact {
        &self.artifact
    }

    pub(crate) fn executable(&self) -> &ExecutablePlasmComp {
        &self.executable
    }

    pub fn into_artifact(self) -> PlasmCompArtifact {
        self.artifact
    }
}

impl TryFrom<PlasmCompArtifact> for PlasmCompBundle {
    type Error = PlasmCompBundleError;

    fn try_from(artifact: PlasmCompArtifact) -> Result<Self, Self::Error> {
        Self::new(artifact)
    }
}
