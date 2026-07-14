//! Validate structured seed-set selector output (invalid → routing_error).

mod resolve;
mod rewriter;
mod types;
mod validation;

#[cfg(test)]
mod tests;

pub use resolve::{resolve_llm_seed_selection, LlmSeedSelectionInput};
pub use rewriter::{
    apply_seed_invariants, apply_seed_invariants_protected, supporting_capabilities_from_bundles,
};
pub use types::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};
pub use validation::{
    seeds_from_candidate_ids, validate_seed_selection, validation_error_label,
    SeedSelectionValidationError, ValidatedAbstainSeedSelection, ValidatedReadySeedSelection,
    ValidatedSeedSelection,
};
