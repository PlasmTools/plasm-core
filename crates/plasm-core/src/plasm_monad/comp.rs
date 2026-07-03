use super::bind_graph::PlasmBindGraph;
use super::payload::PlasmStepPayload;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Program step identifier (binding label / synthetic node id).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(pub String);

impl StepId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("StepId must be non-empty".into());
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
pub const PLASM_COMP_WIRE_VERSION: u32 = 1;

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
    pub fn validate(&self) -> Result<(), String> {
        if self.version != PLASM_COMP_WIRE_VERSION {
            return Err(format!(
                "PlasmComp: version must be {PLASM_COMP_WIRE_VERSION} (got {})",
                self.version
            ));
        }
        if self.steps.is_empty() {
            return Err("PlasmComp: steps must be non-empty".into());
        }
        if self.bind.topo.is_empty() {
            return Err("PlasmComp: bind.topo must be non-empty".into());
        }
        for id in &self.bind.topo {
            if !self.steps.contains_key(id.as_str()) {
                return Err(format!("PlasmComp: bind.topo references unknown step {id}"));
            }
        }
        self.bind.validate(&self.steps.keys().cloned().collect())?;
        Ok(())
    }

    pub fn topological_order(&self) -> &[StepId] {
        &self.bind.topo
    }
}
