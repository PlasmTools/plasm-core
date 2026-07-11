//! Core seed-selection types.

use serde::{Deserialize, Serialize};

/// Selector decision branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedSelectionDecision {
    Ready,
    Clarify,
    HardMiss,
}

/// Parsed selector output before validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedSelectionRaw {
    pub decision: SeedSelectionDecision,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub selected_ids: Vec<String>,
    #[serde(default)]
    pub supporting_capability_ids: Vec<String>,
    #[serde(default)]
    pub alternative_sets: Vec<SeedAlternativeSetRaw>,
    #[serde(default)]
    pub uncovered_requirements: Vec<String>,
    #[serde(default)]
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedAlternativeSetRaw {
    #[serde(default)]
    pub candidate_ids: Vec<String>,
    #[serde(default)]
    pub label: String,
}
