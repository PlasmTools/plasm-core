//! Graph+role witness prune — catalog-authored `seed_nav` / `seed_class` only.
//!
//! Passes are pure `Selected → Selected` stages over a shared corpus (roles precomputed).

mod lone_attach;
mod support;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use super::corpus::WitnessCorpus;

use lone_attach::prefer_lone_attach_mutate_over_parents;
use support::{
    demote_batch_attach_mutations_beside_primary, demote_lone_ambient_to_own_primary,
    drop_redundant_attach, drop_redundant_locate_ambient, drop_redundant_own_targets,
    promote_orphan_attach_reads, promote_orphan_own_target_reads, promote_shared_attach_mutations,
};

/// How prefer_lone consults authored discovery aliases.
///
/// `Strict` is the Ready production path. `Ungated` is the FO-hazard demotion used by
/// unit tests that lock the pre-naming prefer_lone behavior — never wire it in prod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentGate<'a> {
    Strict(&'a str),
    Ungated,
}

/// Drop redundant attach/ambient/own-target witnesses and promote orphan attach reads.
///
/// Uses only roles stamped on witnesses at corpus build time (catalog-authored)
/// plus typed pool links / capability kinds — never entity English.
///
/// `own` XOR: when DirectCapabilities on both ends of an `own` edge are selected,
/// drop Target **reads** and keep Source (mutators on Target remain). Ambient
/// own-sources invert that preference.
///
/// Prefer-lone parent demotion is gated by [`IntentGate`].
pub fn prune_witness_selection(
    corpus: &WitnessCorpus,
    selected: &[usize],
    intent: IntentGate<'_>,
) -> Vec<usize> {
    if selected.is_empty() {
        return Vec::new();
    }
    // Ordered prune liturgy (roles live on the corpus).
    let mut selected = drop_redundant_attach(corpus, selected);
    selected = drop_redundant_own_targets(corpus, &selected);
    selected = drop_redundant_locate_ambient(corpus, &selected);
    selected = promote_orphan_attach_reads(corpus, &selected);
    selected = promote_orphan_own_target_reads(corpus, &selected);
    selected = demote_lone_ambient_to_own_primary(corpus, &selected);
    selected = prefer_lone_attach_mutate_over_parents(corpus, &selected, intent);
    selected = demote_batch_attach_mutations_beside_primary(corpus, &selected);
    selected = promote_shared_attach_mutations(corpus, &selected);
    selected
}
