//! Validate structured seed-set selector output (invalid → routing_error).

mod index;
mod reducer;
mod rewriter;
mod types;
mod validation;

#[cfg(test)]
mod tests;

pub use index::{
    build_seed_bundle_index_tables, build_seed_bundle_provider_groups,
    seed_bundle_presentation_order, SeedBundleIndexTables, SeedBundleProviderGroup,
};
pub use reducer::resolve_seed_coverage_assessment;
pub use types::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};
pub use validation::{
    seeds_from_candidate_ids, validate_seed_selection, validation_error_label,
    SeedSelectionValidationError, ValidatedAbstainSeedSelection, ValidatedReadySeedSelection,
    ValidatedSeedSelection,
};
