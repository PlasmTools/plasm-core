//! Capability-input legend model for teaching-table Meaning tails.

use serde::{Deserialize, Serialize};

use super::TEACHING_OPTIONAL_LEGEND_MARK;

/// Compact optional-invoke legend in the Meaning column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptionalLegend {
    #[default]
    Absent,
    Present,
}

impl OptionalLegend {
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }

    fn is_absent(v: &Self) -> bool {
        !v.is_present()
    }
}

impl<'de> Deserialize<'de> for OptionalLegend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(if s.is_empty() {
            Self::Absent
        } else {
            Self::Present
        })
    }
}

impl Serialize for OptionalLegend {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Absent => serializer.serialize_str(""),
            Self::Present => serializer.serialize_str(TEACHING_OPTIONAL_LEGEND_MARK),
        }
    }
}

/// Scope / optional / compact-args tail for capability teaching rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilityInputLegend {
    #[serde(default)]
    pub scope: String,
    #[serde(
        default,
        rename = "optional_params",
        skip_serializing_if = "OptionalLegend::is_absent"
    )]
    pub optional: OptionalLegend,
    #[serde(default)]
    pub compact_args: String,
    #[serde(default)]
    pub description: String,
}

impl CapabilityInputLegend {
    pub fn set_optional_present(&mut self) {
        self.optional = OptionalLegend::Present;
    }

    pub fn optional_tsv_mark(&self) -> &'static str {
        if self.optional.is_present() {
            TEACHING_OPTIONAL_LEGEND_MARK
        } else {
            ""
        }
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
