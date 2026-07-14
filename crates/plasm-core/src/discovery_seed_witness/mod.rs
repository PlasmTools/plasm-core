//! Closed requirement witnesses + deterministic seed-plan construction.
//!
//! Pipeline:
//! 1. Derive closed `w#` witnesses from the search/graph candidate pool.
//! 2. LLM selects required witnesses (or abstains).
//! 3. Rust enumerates minimal 1–3 seed plans that cover those witnesses.
//! 4. If multiple complete plans remain, LLM does order-swapped pairwise compare;
//!    disagreement → clarify (never silent rewrite).
//!
//! Seed roles are typed stamps (`SeedClassStamp` / `SeedNavStamp` / `OwnPairs` /
//! `PoolLinks`) from catalog-authored `seed_nav` / `seed_class`, plus
//! deterministic graph+role prune — never entity English tables. BAML sees
//! strings only at the presentation boundary.

mod corpus;
mod outcome;
mod plans;
mod prune;
mod roles;

#[cfg(test)]
mod tests;

pub use corpus::{
    build_witness_corpus, RequirementWitness, WitnessCorpus, WitnessKind, MAX_WITNESSES,
    MAX_WITNESS_CATALOGS_UNBRANDED,
};
pub use roles::{
    OwnEdge, OwnEnd, OwnPairs, PoolChild, PoolLinks, SeedClassStamp, SeedNavStamp,
};
pub use outcome::{
    missing_named_catalog_coverage, selection_clarify_from_plans, selection_from_plan,
    selection_hard_miss, synthesize_clarify_alternatives,
};
pub use plans::{
    construct_minimal_plans, shortlist_plans, verify_plan, DeterministicSeedPlan,
    PlanConstructError, MAX_COMPARE_PLANS,
};
pub use prune::prune_witness_selection;
