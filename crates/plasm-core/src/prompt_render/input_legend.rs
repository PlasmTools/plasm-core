//! Capability-input legend model for teaching-table Meaning tails.

use serde::{Deserialize, Serialize};

/// Scope / optional / compact-args tail for capability teaching rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilityInputLegend {
    #[serde(default)]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_params: Vec<String>,
    #[serde(default)]
    pub compact_args: String,
    #[serde(default)]
    pub description: String,
}

impl CapabilityInputLegend {
    pub fn optional_params_present(&self) -> bool {
        !self.optional_params.is_empty()
    }
}

/// Row-producer input/projection contract (typed; not parsed from expr at emit).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RowContractLegend {
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub rows: RowProjectionContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RowProjectionContract {
    #[default]
    Absent,
    OmittedSameAsWitness,
    Explicit {
        syms: Vec<String>,
    },
}

/// One synthesized teaching-table expression row (model → TSV).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingExprLine {
    pub expression: String,
    pub result_type: String,
    #[serde(flatten)]
    pub legend: CapabilityInputLegend,
    pub is_projection_teaching: bool,
    #[serde(default)]
    pub row_contract: RowContractLegend,
}

impl TeachingExprLine {
    pub fn empty_legend(expression: String) -> Self {
        Self {
            expression,
            result_type: String::new(),
            legend: CapabilityInputLegend::default(),
            is_projection_teaching: false,
            row_contract: RowContractLegend::default(),
        }
    }
}
