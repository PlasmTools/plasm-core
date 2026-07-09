//! Capability-input legend model for teaching-table Meaning tails.

use serde::{Deserialize, Serialize};

/// Return-shape glyph for a teaching row's `Meaning` result atom.
///
/// The glyph is chosen at the arrow so an agent can read *chainability* directly from the arrow:
/// - [`ReturnArrow::Single`] `→` — one record; a chainable anchor (`.r#` / `.m#` / get-head reuse).
/// - [`ReturnArrow::List`] `↣` — a list of rows; chainable via postfix (`.filter{…}` / `.sort` / `[field,…]`).
/// - [`ReturnArrow::Terminal`] `↠` — a terminal write result (or unit `()`); **not** an expression
///   anchor. To keep operating, reconstruct the entity with a get (`e#(id=…)`) then chain `.m#`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReturnArrow {
    #[default]
    Single,
    List,
    Terminal,
}

impl ReturnArrow {
    /// Unicode arrow glyph rendered before the result gloss in the `Meaning` column.
    pub const fn glyph(self) -> &'static str {
        match self {
            ReturnArrow::Single => "→",
            ReturnArrow::List => "↣",
            ReturnArrow::Terminal => "↠",
        }
    }

    /// Classify the return shape from the domain-line kind and result gloss.
    ///
    /// Writes (`Method`) are terminal regardless of gloss (`e#` provides slice or `()`). Query /
    /// search are lists. Everything else falls back to gloss shape (`[…]` list vs single).
    pub fn classify(kind: crate::prompt_render::DomainLineKind, gloss: &str) -> Self {
        use crate::prompt_render::DomainLineKind as K;
        match kind {
            K::Method => ReturnArrow::Terminal,
            K::Query | K::Search => ReturnArrow::List,
            _ if gloss.trim_start().starts_with('[') => ReturnArrow::List,
            _ => ReturnArrow::Single,
        }
    }
}

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
    /// Return-shape glyph for the result atom (`→` / `↣` / `↠`). Set from the validated
    /// domain-line kind at push time; defaults to [`ReturnArrow::Single`].
    #[serde(default)]
    pub arrow: ReturnArrow,
}

impl TeachingExprLine {
    pub fn empty_legend(expression: String) -> Self {
        Self {
            expression,
            result_type: String::new(),
            legend: CapabilityInputLegend::default(),
            is_projection_teaching: false,
            row_contract: RowContractLegend::default(),
            arrow: ReturnArrow::Single,
        }
    }
}
