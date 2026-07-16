//! Mutator admission for intent-filtered exposure surfaces.
//!
//! Seeded mutators under [`MutatorAdmit::IntentOnly`]: BM25 score > 0 **or** ranked wire boost.
//! Non-seeded relation-target mutators: score required; ranked is a whitelist when non-empty.

use crate::schema::{CapabilityKind, CapabilitySchema, EntityDef};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[cfg(feature = "ranked_capability_gate")]
fn ranked_lists_cap(ranked_capability_names: Option<&[String]>, cap_name: &str) -> bool {
    match ranked_capability_names {
        None | Some([]) => false,
        Some(names) => names.iter().any(|n| n.as_str() == cap_name),
    }
}

#[cfg(feature = "ranked_capability_gate")]
fn ranked_whitelist_allows(ranked_capability_names: Option<&[String]>, cap_name: &str) -> bool {
    match ranked_capability_names {
        None | Some([]) => true,
        Some(names) => names.iter().any(|n| n.as_str() == cap_name),
    }
}

/// How seeded-entity mutators (`create`/`update`/`delete`/`action`) are admitted on an exposure wave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutatorAdmit {
    /// Production default: seeded reads always taught; seeded mutators admit via lexicon **or**
    /// ranked boost (union). Non-seeded relation-target mutators stay strict (score + optional
    /// ranked whitelist).
    #[default]
    IntentOnly,
    /// Test/benchmark overshow: seeded mutators always admitted.
    AlwaysOnSeeds,
}

/// Options for [`super::derive_intent_exposure_surface_batch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExposureSurfaceOptions {
    /// Production ([`MutatorAdmit::IntentOnly`]): seeded entities always teach reads
    /// (`query`/`search`/`get` + `primary_read`); seeded mutators admit when BM25 score > 0 **or**
    /// the wire name is listed in `ranked_capability_names` (score-0 boost). Ranked is a boost on
    /// seeded domains — not a whitelist cage. Non-seeded relation-target mutators require score > 0
    /// and, when ranked is non-empty, membership in that list.
    /// [`MutatorAdmit::AlwaysOnSeeds`] always admits seeded mutators (tests / overshow).
    pub mutator_admit: MutatorAdmit,
}

/// Seeded mutator admit: lexicon **or** ranked boost (union).
pub(crate) fn seeded_mutating_capability_admitted(
    score: u32,
    ranked_capability_names: Option<&[String]>,
    cap_name: &str,
) -> bool {
    if score > 0 {
        return true;
    }
    #[cfg(feature = "ranked_capability_gate")]
    {
        ranked_lists_cap(ranked_capability_names, cap_name)
    }
    #[cfg(not(feature = "ranked_capability_gate"))]
    {
        let _ = (ranked_capability_names, cap_name);
        false
    }
}

/// Non-seeded / relation-target mutator admit: score required; ranked is a whitelist when set.
pub(crate) fn mutating_capability_admitted(
    score: u32,
    ranked_capability_names: Option<&[String]>,
    cap_name: &str,
) -> bool {
    if score == 0 {
        return false;
    }
    #[cfg(feature = "ranked_capability_gate")]
    {
        ranked_whitelist_allows(ranked_capability_names, cap_name)
    }
    #[cfg(not(feature = "ranked_capability_gate"))]
    {
        let _ = ranked_capability_names;
        let _ = cap_name;
        true
    }
}

/// Capabilities on an explicitly seeded entity that are always admitted (no intent lexicon score).
pub(crate) fn seeded_entity_cap_always_includes(
    mutator_admit: MutatorAdmit,
    cap: &CapabilitySchema,
    entity_name: &str,
    ent: &EntityDef,
    seeded_entities: &HashSet<String>,
) -> bool {
    if cap.domain.as_str() != entity_name || !seeded_entities.contains(entity_name) {
        return false;
    }
    if matches!(
        cap.kind,
        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
    ) {
        return true;
    }
    if ent
        .primary_read
        .as_deref()
        .is_some_and(|pr| pr == cap.name.as_str())
    {
        return true;
    }
    matches!(mutator_admit, MutatorAdmit::AlwaysOnSeeds)
        && matches!(
            cap.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        )
}
