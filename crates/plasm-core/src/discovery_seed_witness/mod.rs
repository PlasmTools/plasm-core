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
//!
//! Prune is a fixed pass liturgy over [`CorpusRoleIndex`].
//! Cover seating vs teaching is sealed as [`CoverPolicy`]. Kind string soup
//! collapses through [`CapBucket`].

mod corpus;
mod kind;
mod named_in_intent;
mod outcome;
mod plans;
mod prune;
mod role_index;
mod roles;
mod satellites;

#[cfg(test)]
mod tests;

pub use corpus::{
    build_witness_corpus, RequirementWitness, WitnessCorpus, WitnessKind, MAX_WITNESSES,
    MAX_WITNESS_CATALOGS_UNBRANDED,
};
pub use kind::CapBucket;
pub use outcome::{
    missing_named_catalog_coverage, selection_clarify_from_plans, selection_from_plan,
    selection_hard_miss, synthesize_clarify_alternatives,
};
pub use plans::{
    construct_minimal_plans, construct_minimal_plans_with_cover, construct_workflow_seed_plans,
    covering, prefer_primary_cover_plan, shortlist_plans, verify_plan, verify_plan_with_cover,
    CoverMode, CoverPolicy, DeterministicSeedPlan, PlanConstructError, MAX_COMPARE_PLANS,
};
pub use prune::{prune_witness_selection, IntentGate};
pub use role_index::CorpusRoleIndex;
pub use roles::{OwnEdge, OwnEnd, OwnPairs, PoolChild, PoolLinks, SeedClassStamp, SeedNavStamp};
pub use satellites::{
    admit_teaching_satellites, apply_teaching_satellites_to_ready, candidates_covering_for_plan,
    candidates_covering_with_satellites, dependent_action_shadowed_by_peer_primary,
    is_attach_or_dependent_leaf, seed_candidate_is_teaching_leaf, SatelliteAdmission,
    MAX_TEACHING_SATELLITES,
};
