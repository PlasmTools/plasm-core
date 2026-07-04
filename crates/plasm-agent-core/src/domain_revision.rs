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

#[cfg(test)]
mod tests {
    use super::*;

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
}
