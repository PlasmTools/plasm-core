//! Resolve symbol-tuned LLM seed selection output into validated raw selection.
//!
//! Ready path is membership-only (symbols → candidate ids). Semantic rewriter
//! stays behind coverage `postprocess_coverage_selection` for eval shadow.

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::discovery_seed_symbol_map::SeedSymbolMap;

use super::rewriter::supporting_capabilities_from_bundles;
use super::types::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};
use super::validation::SeedSelectionValidationError;

/// Parsed LLM assessment before symbol resolution (BAML-agnostic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSeedSelectionInput {
    pub decision: SeedSelectionDecision,
    pub selected_symbols: Vec<String>,
    pub requirements: Vec<String>,
    pub alternative_symbol_sets: Vec<Vec<String>>,
    pub alternative_labels: Vec<String>,
    pub uncovered_requirements: Vec<String>,
    pub reasoning: String,
}

pub fn resolve_llm_seed_selection(
    input: LlmSeedSelectionInput,
    symbol_map: &SeedSymbolMap,
    bundles: &[EntityCandidateBundle],
    _intent_class: &crate::discovery_intent_class::DiscoveryIntentClass,
    _catalog_context: Option<&CatalogWorkflowContext>,
    _candidate_graph: Option<&crate::discovery_candidate_graph::TypedCandidateGraph>,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    match input.decision {
        SeedSelectionDecision::Ready => {
            let selected_ids = symbol_map.resolve_symbols(&input.selected_symbols)?;
            let supporting_capability_ids =
                supporting_capabilities_from_bundles(&selected_ids, bundles);
            Ok(SeedSelectionRaw {
                decision: SeedSelectionDecision::Ready,
                requirements: input.requirements,
                selected_ids,
                supporting_capability_ids,
                teaching_satellites: Vec::new(),
                alternative_sets: Vec::new(),
                uncovered_requirements: Vec::new(),
                reasoning: input.reasoning,
            })
        }
        SeedSelectionDecision::Clarify => {
            if !input.selected_symbols.is_empty() {
                return Err(SeedSelectionValidationError::ClarifySelectedSeeds);
            }
            let resolved_sets =
                symbol_map.resolve_alternative_symbol_sets(&input.alternative_symbol_sets)?;
            if resolved_sets.len() < 2 {
                return Err(SeedSelectionValidationError::ClarifyMissingAlternatives);
            }
            let alternative_sets = resolved_sets
                .into_iter()
                .enumerate()
                .map(|(index, candidate_ids)| SeedAlternativeSetRaw {
                    candidate_ids,
                    label: input
                        .alternative_labels
                        .get(index)
                        .cloned()
                        .unwrap_or_default(),
                })
                .collect();
            Ok(SeedSelectionRaw {
                decision: SeedSelectionDecision::Clarify,
                requirements: input.requirements,
                selected_ids: Vec::new(),
                supporting_capability_ids: Vec::new(),
                teaching_satellites: vec![],
                alternative_sets,
                uncovered_requirements: input.uncovered_requirements,
                reasoning: input.reasoning,
            })
        }
        SeedSelectionDecision::HardMiss => {
            if !input.selected_symbols.is_empty() {
                return Err(SeedSelectionValidationError::HardMissSelectedSeeds);
            }
            if input.uncovered_requirements.is_empty() {
                return Err(SeedSelectionValidationError::HardMissMissingUncovered);
            }
            Ok(SeedSelectionRaw {
                decision: SeedSelectionDecision::HardMiss,
                requirements: input.requirements,
                selected_ids: Vec::new(),
                supporting_capability_ids: Vec::new(),
                teaching_satellites: vec![],
                alternative_sets: Vec::new(),
                uncovered_requirements: input.uncovered_requirements,
                reasoning: input.reasoning,
            })
        }
    }
}
