use super::bind_graph::PlasmBindGraph;
use super::payload::PlasmStepPayload;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlasmCompValidationError {
    #[error("PlasmComp version must be {expected} (got {actual})")]
    UnsupportedVersion { expected: u32, actual: u32 },
    #[error("PlasmComp steps must be non-empty")]
    EmptySteps,
    #[error("PlasmComp bind.topo must be non-empty")]
    EmptyTopology,
    #[error("PlasmComp bind.topo references unknown step `{step}`")]
    UnknownTopologicalStep { step: String },
    #[error("iteration step differs from its sealed effect contract")]
    IterationEffectContractMismatch,
    #[error("iteration step omits a captured dependency `{step}` -> `{dependency}`")]
    MissingIterationDependency { step: String, dependency: String },
    #[error("iteration predicate must be a singleton filter over its seed")]
    InvalidIterationPredicate,
    #[error("map body `{step}` omits parent dependency `{dependency}`")]
    MissingMapBodyParentDependency { step: String, dependency: String },
    #[error(transparent)]
    BindGraph(#[from] super::bind_graph::BindGraphError),
    #[error(transparent)]
    CorrelatedBody(#[from] super::correlated::CorrelatedBodyError),
    #[error(transparent)]
    IterationEffect(#[from] super::correlated::IterationStepEffectError),
}

/// Program step identifier (binding label / synthetic node id).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(pub String);

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum StepIdError {
    #[error("step identifier must not be empty")]
    Empty,
}

impl StepId {
    pub fn new(value: impl Into<String>) -> Result<Self, StepIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StepIdError::Empty);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Applicative product at return (parallel roots) or single bind-chain result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlasmReturn {
    Step { step: StepId },
    Parallel { steps: Vec<StepId> },
}

/// Canonical PlasmComp wire version (`_meta.plasm.comp.version`).
pub const PLASM_COMP_WIRE_VERSION: u32 = 2;

/// Canonical executable Plasm program (wire + in-memory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlasmComp {
    #[serde(default = "default_comp_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub steps: BTreeMap<String, PlasmStepPayload>,
    pub bind: PlasmBindGraph,
    #[serde(rename = "return")]
    pub return_: PlasmReturn,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

fn default_comp_version() -> u32 {
    PLASM_COMP_WIRE_VERSION
}

/// Validated comp ready for dry-run / live execution.
#[derive(Debug, Clone)]
pub struct PlasmCompArtifact {
    pub comp: PlasmComp,
    pub approval_gates: Vec<StepId>,
}

impl PlasmComp {
    pub fn validate(&self) -> Result<(), PlasmCompValidationError> {
        if self.version != PLASM_COMP_WIRE_VERSION {
            return Err(PlasmCompValidationError::UnsupportedVersion {
                expected: PLASM_COMP_WIRE_VERSION,
                actual: self.version,
            });
        }
        if self.steps.is_empty() {
            return Err(PlasmCompValidationError::EmptySteps);
        }
        if self.bind.topo.is_empty() {
            return Err(PlasmCompValidationError::EmptyTopology);
        }
        for id in &self.bind.topo {
            if !self.steps.contains_key(id.as_str()) {
                return Err(PlasmCompValidationError::UnknownTopologicalStep {
                    step: id.as_str().to_owned(),
                });
            }
        }
        self.bind.validate(&self.steps.keys().cloned().collect())?;
        for (id, payload) in &self.steps {
            if let PlasmStepPayload::UnfoldUntil(unfold) = payload {
                if let Some(body) = &unfold.step_scope {
                    let effect = super::correlated::iteration_step_effect(body)?;
                    if body.parent.source.as_str() != unfold.source
                        || effect.kind != unfold.effect_template.kind
                        || effect.qualified_entity != unfold.effect_template.qualified_entity
                        || effect.ir_template.expr != unfold.effect_template.ir_template.expr
                        || effect.ir_template.projection
                            != unfold.effect_template.ir_template.projection
                        || effect.effect_class != unfold.effect_template.effect_class
                        || effect.result_shape != unfold.effect_template.result_shape
                    {
                        return Err(PlasmCompValidationError::IterationEffectContractMismatch);
                    }
                    let deps = self.bind.deps.get(&StepId(id.clone()));
                    for source in std::iter::once(&body.parent.source)
                        .chain(body.captures.iter().map(|c| &c.source))
                    {
                        if !deps.is_some_and(|deps| deps.contains(source)) {
                            return Err(PlasmCompValidationError::MissingIterationDependency {
                                step: id.clone(),
                                dependency: source.as_str().to_owned(),
                            });
                        }
                    }
                }
                if let Some(body) = &unfold.until_scope {
                    body.execution_layers()?;
                    if !matches!(body.output, super::ScopedOutput::Filter)
                        || body.parent.source.as_str() != unfold.source
                        || body.max_parents.get() != 1
                        || !unfold.until_predicates.is_empty()
                    {
                        return Err(PlasmCompValidationError::InvalidIterationPredicate);
                    }
                    let deps = self.bind.deps.get(&StepId(id.clone()));
                    for source in std::iter::once(&body.parent.source)
                        .chain(body.captures.iter().map(|c| &c.source))
                    {
                        if !deps.is_some_and(|deps| deps.contains(source)) {
                            return Err(PlasmCompValidationError::MissingIterationDependency {
                                step: id.clone(),
                                dependency: source.as_str().to_owned(),
                            });
                        }
                    }
                }
            }
            if let PlasmStepPayload::MapBody(body) = payload {
                body.execution_layers()?;
                if !self
                    .bind
                    .deps
                    .get(&StepId(id.clone()))
                    .is_some_and(|deps| deps.contains(&body.parent.source))
                {
                    return Err(PlasmCompValidationError::MissingMapBodyParentDependency {
                        step: id.clone(),
                        dependency: body.parent.source.as_str().to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn topological_order(&self) -> &[StepId] {
        &self.bind.topo
    }
}
