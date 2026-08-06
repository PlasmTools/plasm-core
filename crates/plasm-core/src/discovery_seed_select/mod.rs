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
    classify_clarify, seeds_from_candidate_ids, try_federation_ready_under_brand_lock,
    validate_seed_selection, validate_seed_selection_with_brand_lock, validation_error_label,
    ClarifyKind, SeedSelectionValidationError, ValidatedAbstainSeedSelection,
    ValidatedReadySeedSelection, ValidatedSeedSelection,
};
