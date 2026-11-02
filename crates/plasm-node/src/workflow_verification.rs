//! Host-only compatibility reporting through ordinary example serving.
use std::{collections::BTreeMap, sync::Arc};

use plasm_agent_core::{
    discovery_store::DiscoverySessionPin,
    python_pool::PythonPool,
    workflow_learning::{model::LibraryScope, native::NativeLearningConfig, serving::ServingError},
};
use serde::Serialize;

pub(crate) const MAX_VERIFIED_EXAMPLES: u32 = 32;

/// One activated generation, frozen for the entire administrative operation.
pub(crate) struct AdministrativeGeneration {
    pub catalogs: BTreeMap<String, plasm_core::CGS>,
    pub compiled: BTreeMap<String, Arc<plasm_compile::CompiledCatalog>>,
    pub pin: DiscoverySessionPin,
    pub deployments: plasm_core::prerequisites::DeploymentBindings,
}
impl AdministrativeGeneration {
    pub fn engine(
        &self,
        config: NativeLearningConfig,
        pool: Arc<PythonPool>,
    ) -> Result<crate::engine::AgentEngine, crate::engine::AgentEngineError> {
        let mut pin = self.pin.clone();
        pin.pin_id = plasm_agent_core::session_identity::LogicalSessionId::new_v4().to_string();
        let mut engine = crate::engine::AgentEngine::from_pinned_generation(
            self.catalogs.clone(),
            self.compiled.clone(),
            pin,
            self.deployments.clone(),
            pool,
        );
        engine.configure_workflow_learning(config)?;
        Ok(engine)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct LibraryVerification {
    pub scope: LibraryScope,
    pub generation: String,
    pub limit: u32,
    pub truncated: bool,
    pub examples: Vec<ExampleVerification>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ExampleVerification {
    pub example_ref: String,
    pub outcome: CompatibilityOutcome,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CompatibilityOutcome {
    Checked {
        source_sha256: String,
        domain_revision: u32,
    },
    Ineligible {
        reason: EligibilityReason,
    },
    Incompatible {
        reason: IncompatibilityReason,
    },
    Unverified {
        reason: VerificationFailure,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EligibilityReason {
    AuthorizationOrContext,
    Prerequisite,
    Overlay,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IncompatibilityReason {
    TeachingContract,
    ProgramAdmission,
    InvalidExample,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationFailure {
    Repository,
    ContextChanged,
    CataloguePreparation,
    CompilationInfrastructure,
}

impl CompatibilityOutcome {
    pub fn from_serving_error(error: ServingError) -> Self {
        match error {
            ServingError::Inapplicable | ServingError::ContextRequired => Self::Ineligible {
                reason: EligibilityReason::AuthorizationOrContext,
            },
            ServingError::Prerequisite(_) => Self::Ineligible {
                reason: EligibilityReason::Prerequisite,
            },
            ServingError::OverlayRequired { .. } => Self::Ineligible {
                reason: EligibilityReason::Overlay,
            },
            ServingError::Teaching(_) => Self::Incompatible {
                reason: IncompatibilityReason::TeachingContract,
            },
            ServingError::Invalid(_) => Self::Incompatible {
                reason: IncompatibilityReason::InvalidExample,
            },
            ServingError::Compilation(
                plasm_agent_core::compilation_error::CompilationError::Program(_),
            ) => Self::Incompatible {
                reason: IncompatibilityReason::ProgramAdmission,
            },
            ServingError::Compilation(_) => Self::Unverified {
                reason: VerificationFailure::CompilationInfrastructure,
            },
            ServingError::Repository(_) => Self::Unverified {
                reason: VerificationFailure::Repository,
            },
            ServingError::Conflict | ServingError::Persistence(_) => Self::Unverified {
                reason: VerificationFailure::ContextChanged,
            },
            ServingError::Catalog(_)
            | ServingError::Discovery(_)
            | ServingError::CapabilityExposure(_)
            | ServingError::PythonTeaching(_)
            | ServingError::ValueDomain(_)
            | ServingError::Session(_) => Self::Unverified {
                reason: VerificationFailure::CataloguePreparation,
            },
        }
    }
}
