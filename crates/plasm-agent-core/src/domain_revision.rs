//! Append-only exposure revision policy for execute sessions and plan commits.
//!
//! Symbol ledgers only grow: a plan compiled at revision *N* remains valid after extend
//! (`session.rev > N`). Plans pinned *ahead* of the session row are split-brain and must
//! not run or merge onto that row.

/// Monotonic exposure wave counter on an execute session / plan commit.
///
/// Newtype prevents swapping plan vs session arguments at policy call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DomainRevision(u32);

impl DomainRevision {
    #[inline]
    pub const fn new(v: u32) -> Self {
        Self(v)
    }

    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for DomainRevision {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl std::fmt::Display for DomainRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Plan is valid on this session when its exposure is a prefix of the live ledger.
#[inline]
pub fn plan_compatible_with_session(plan: DomainRevision, session: DomainRevision) -> bool {
    plan <= session
}

/// Hot vs durable exposure relationship for persist / merge decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposureSync {
    /// Hot `domain_revision` is behind durable — rehydrate before writing.
    HotBehind,
    /// Revisions match — plan-only patch is safe.
    InSync,
    /// Hot is ahead of durable — full-persist hot (authoritative).
    HotAhead,
}

#[inline]
pub fn compare_exposure(hot: DomainRevision, durable: DomainRevision) -> ExposureSync {
    use std::cmp::Ordering;
    match hot.cmp(&durable) {
        Ordering::Less => ExposureSync::HotBehind,
        Ordering::Equal => ExposureSync::InSync,
        Ordering::Greater => ExposureSync::HotAhead,
    }
}

/// Durable write strategy from hot vs durable exposure revision + materialization inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposurePersistPolicy {
    /// Hot is behind durable — refuse writes until rehydrate.
    RefuseHotBehind {
        hot_revision: u32,
        durable_revision: u32,
    },
    /// Revisions match — patch plan commits (or bind creds) without rematerializing catalogs.
    PatchMetadataOnly,
    /// Hot ahead but catalog materialization inputs unchanged — sync hot exposure fields using
    /// existing durable effective pins (skip per-entry rematerialize).
    SyncHotExposureReusePins,
    /// Hot ahead with changed federation/materialization inputs — full rematerialize + persist.
    FullPersistMaterialize,
}

/// Materialization inputs compared when hot exposure is ahead of durable during plan commit.
pub struct PlanCommitMaterializationInputs<'a> {
    pub session_context_entry_ids: &'a [String],
    pub durable_context_entry_ids: &'a [String],
    pub session_outbound: &'a std::collections::HashMap<String, String>,
    pub durable_outbound: &'a std::collections::HashMap<String, String>,
    pub session_bindings_len: usize,
    pub durable_bindings_len: usize,
}

/// Plan-commit durable patch: avoid rematerializing every catalog row when hot is ahead but
/// tenant outbound KV, bindings, and loaded entry set match the durable row.
pub fn plan_commit_persist_policy(
    hot: DomainRevision,
    durable: DomainRevision,
    inputs: PlanCommitMaterializationInputs<'_>,
) -> ExposurePersistPolicy {
    match compare_exposure(hot, durable) {
        ExposureSync::HotBehind => ExposurePersistPolicy::RefuseHotBehind {
            hot_revision: hot.get(),
            durable_revision: durable.get(),
        },
        ExposureSync::InSync => ExposurePersistPolicy::PatchMetadataOnly,
        ExposureSync::HotAhead => {
            let same_entries =
                inputs.session_context_entry_ids == inputs.durable_context_entry_ids;
            let same_outbound = inputs.session_outbound == inputs.durable_outbound;
            let same_bindings = inputs.session_bindings_len == inputs.durable_bindings_len;
            if same_entries && same_outbound && same_bindings {
                ExposurePersistPolicy::SyncHotExposureReusePins
            } else {
                ExposurePersistPolicy::FullPersistMaterialize
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn plan_compatible_is_prefix_order() {
        assert!(plan_compatible_with_session(
            DomainRevision::new(0),
            DomainRevision::new(0)
        ));
        assert!(plan_compatible_with_session(
            DomainRevision::new(0),
            DomainRevision::new(1)
        ));
        assert!(plan_compatible_with_session(
            DomainRevision::new(2),
            DomainRevision::new(3)
        ));
        assert!(!plan_compatible_with_session(
            DomainRevision::new(3),
            DomainRevision::new(2)
        ));
    }

    #[test]
    fn compare_exposure_trichotomy() {
        assert_eq!(
            compare_exposure(DomainRevision::new(1), DomainRevision::new(2)),
            ExposureSync::HotBehind
        );
        assert_eq!(
            compare_exposure(DomainRevision::new(2), DomainRevision::new(2)),
            ExposureSync::InSync
        );
        assert_eq!(
            compare_exposure(DomainRevision::new(3), DomainRevision::new(2)),
            ExposureSync::HotAhead
        );
    }

    #[test]
    fn plan_commit_policy_hot_ahead_reuses_pins_when_inputs_unchanged() {
        let outbound = HashMap::from([("github".into(), "kv".into())]);
        let session_entries = ["github".into()];
        assert_eq!(
            plan_commit_persist_policy(
                DomainRevision::new(3),
                DomainRevision::new(1),
                PlanCommitMaterializationInputs {
                    session_context_entry_ids: &session_entries,
                    durable_context_entry_ids: &session_entries,
                    session_outbound: &outbound,
                    durable_outbound: &outbound,
                    session_bindings_len: 0,
                    durable_bindings_len: 0,
                },
            ),
            ExposurePersistPolicy::SyncHotExposureReusePins
        );
    }

    #[test]
    fn plan_commit_policy_hot_ahead_materializes_when_federation_grew() {
        let session_entries = ["github".into(), "linear".into()];
        let durable_entries = ["github".into()];
        assert_eq!(
            plan_commit_persist_policy(
                DomainRevision::new(2),
                DomainRevision::new(1),
                PlanCommitMaterializationInputs {
                    session_context_entry_ids: &session_entries,
                    durable_context_entry_ids: &durable_entries,
                    session_outbound: &HashMap::new(),
                    durable_outbound: &HashMap::new(),
                    session_bindings_len: 0,
                    durable_bindings_len: 0,
                },
            ),
            ExposurePersistPolicy::FullPersistMaterialize
        );
    }
}
