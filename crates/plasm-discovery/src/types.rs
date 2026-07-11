//! Public request/response types for typed discovery.

use plasm_core::schema::CapabilityKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable reason codes for [`DiscoveryEvidence`] (metrics/evals/UI).
pub mod evidence_codes {
    pub const EXACT_PHRASE: &str = "exact_phrase";
    pub const ENTITY_NAME: &str = "entity_name";
    pub const EXPRESSION_ALIAS: &str = "expression_alias";
    pub const DISCOVERY_NAME: &str = "discovery_name";
    pub const CAP_TARGET_TERM: &str = "cap_target_term";
    pub const TOKEN_OVERLAP: &str = "token_overlap";
    pub const EMBEDDING_SIM: &str = "embedding_similarity";
    pub const GRAPH_QUALIFIER_OK: &str = "graph_qualifier_ok";
    pub const GRAPH_QUALIFIER_BAD: &str = "graph_qualifier_bad";
    pub const RELATION_INTENT: &str = "relation_intent";
    pub const CAMEL_SEGMENT_CONJUNCTION: &str = "camel_segment_conjunction";
}

#[derive(Debug, Clone, Error)]
pub enum DiscoveryError {
    #[error("empty discovery utterance")]
    EmptyUtterance,
    #[error("unknown catalog entry: {0}")]
    UnknownEntry(String),
    #[error("index build failed: {0}")]
    IndexBuild(String),
    #[error("invalid clarification answer")]
    InvalidClarificationAnswer,
}

impl From<plasm_core::discovery::DiscoveryError> for DiscoveryError {
    fn from(e: plasm_core::discovery::DiscoveryError) -> Self {
        match e {
            plasm_core::discovery::DiscoveryError::UnknownEntry(id) => Self::UnknownEntry(id),
            plasm_core::discovery::DiscoveryError::EmptyQuery => {
                Self::IndexBuild("empty discovery query".into())
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveryQuery {
    pub utterance: String,
    #[serde(default)]
    pub allowed_entry_ids: Vec<String>,
    #[serde(default)]
    pub prior_state: Option<ClarificationState>,
    #[serde(default = "default_max_options")]
    pub max_options: usize,
    /// Narrow to one catalog after a clarification step (`answer_clarification`).
    #[serde(default)]
    pub force_entry_id: Option<String>,
    /// Narrow to one CGS entity name after a clarification step.
    #[serde(default)]
    pub force_entity: Option<String>,
}

fn default_max_options() -> usize {
    8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationState {
    pub dimension: ClarificationDimension,
    pub round: u32,
    /// Original user utterance (resubmit on `answer_clarification`).
    pub utterance: String,
    #[serde(default)]
    pub allowed_entry_ids: Vec<String>,
    #[serde(default = "default_max_options")]
    pub max_options: usize,
    #[serde(default)]
    pub options: Vec<ClarificationOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationDimension {
    Api,
    Entity,
    Qualifier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationAnswer {
    pub selected_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposedIntent {
    pub operation_verbs: Vec<String>,
    pub api_hints: Vec<String>,
    pub noun_phrases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryEvidence {
    pub code: String,
    pub detail: String,
}

impl DiscoveryEvidence {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetHypothesis {
    pub entry_id: String,
    pub entity: String,
    pub capability_name: String,
    pub capability_kind: CapabilityKind,
    pub score: f64,
    pub matched_phrase: String,
    pub qualifiers: Vec<String>,
    pub evidence: Vec<DiscoveryEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyTarget {
    pub entry_id: String,
    pub entity: String,
    pub capability_name: String,
    pub capability_kind: CapabilityKind,
    pub score: f64,
    pub matched_phrase: String,
    pub qualifiers: Vec<String>,
    pub evidence: Vec<DiscoveryEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationPrompt {
    pub dimension: ClarificationDimension,
    pub prompt: String,
    pub options: Vec<ClarificationOption>,
    pub evidence: Vec<DiscoveryEvidence>,
}

impl ClarificationPrompt {
    /// Build [`ClarificationState`] for a follow-up `answer_clarification` call.
    pub fn build_state(
        &self,
        round: u32,
        utterance: String,
        allowed_entry_ids: Vec<String>,
        max_options: usize,
    ) -> ClarificationState {
        ClarificationState {
            dimension: self.dimension,
            round,
            utterance,
            allowed_entry_ids,
            max_options,
            options: self.options.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum DiscoveryDecision {
    Ready {
        target: ReadyTarget,
    },
    ClarifyApi {
        prompt: ClarificationPrompt,
    },
    ClarifyEntity {
        prompt: ClarificationPrompt,
    },
    ClarifyQualifier {
        prompt: ClarificationPrompt,
    },
    NoMatch {
        #[serde(default)]
        evidence: Vec<DiscoveryEvidence>,
    },
}
