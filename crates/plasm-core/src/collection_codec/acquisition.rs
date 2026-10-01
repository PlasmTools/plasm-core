//! A single ordered acquisition, distinct from concatenating independent queries.
use super::{
    CollectionFault, CollectionIdentity, Evidence, EvidenceGap, Gap, RecordedCollection,
    ResultCoverage, SharedRows,
};
use std::collections::BTreeSet;

/// A trusted pagination driver's statement about the next page. Empty payloads
/// do not imply exhaustion; an exhausted driver does not undo discarded rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTermination {
    More,
    Exhausted,
    Unproven,
}

/// An adapter records acquisition in order. Payloads are moved into immutable
/// batches; the final record shares those batches. A builder cannot be restored
/// from serialized data or manufactured from a coverage summary.
#[derive(Debug)]
pub struct Acquisition<R> {
    identity: CollectionIdentity,
    batches: Vec<SharedRows<R>>,
    next_page: usize,
    discarded: usize,
    contiguous_prefix: usize,
    termination: PageTermination,
}

impl<R> Acquisition<R> {
    pub(super) fn new(identity: CollectionIdentity) -> Self {
        Self {
            identity,
            batches: Vec::new(),
            next_page: 0,
            discarded: 0,
            contiguous_prefix: 0,
            termination: PageTermination::Unproven,
        }
    }

    /// `decoded` counts the producer's decoded occurrences before a host cap.
    /// `rows` is the retained ordered prefix, validated by the producer adapter.
    /// The expected identity and page ordinal prevent mixing streams, epochs,
    /// duplicate deliveries and gaps. Rejected pages do not change this builder.
    pub fn push(
        &mut self,
        identity: &CollectionIdentity,
        page: usize,
        rows: SharedRows<R>,
        decoded: usize,
        termination: PageTermination,
    ) -> Result<(), CollectionFault> {
        if identity != &self.identity {
            return Err(CollectionFault::IdentityMismatch);
        }
        if page != self.next_page
            || (self.next_page != 0 && self.termination != PageTermination::More)
        {
            return Err(CollectionFault::Conservation);
        }
        let discarded = decoded
            .checked_sub(rows.len())
            .and_then(|n| self.discarded.checked_add(n))
            .ok_or(CollectionFault::Conservation)?;
        let next_page = self
            .next_page
            .checked_add(1)
            .ok_or(CollectionFault::Conservation)?;
        if self.discarded == 0 {
            self.contiguous_prefix = self
                .contiguous_prefix
                .checked_add(rows.len())
                .ok_or(CollectionFault::Conservation)?;
        }
        self.batches.push(rows);
        self.discarded = discarded;
        self.next_page = next_page;
        self.termination = termination;
        Ok(())
    }

    /// A streaming prefix is a distinct expression. Only the uninterrupted
    /// observed prefix can satisfy it; a later page cannot fill an earlier gap.
    pub fn finish_prefix(self, bound: usize) -> Result<RecordedCollection<R>, CollectionFault> {
        let satisfied = self.contiguous_prefix >= bound;
        let mut record = self.finish();
        record.identity = record.identity.derived(&("streaming_prefix", bound))?;
        record.rows = record.rows.select(0..bound.min(record.rows.len()))?;
        if satisfied {
            record.evidence = std::sync::Arc::new(Evidence {
                coverage: ResultCoverage::Complete,
                gaps: BTreeSet::new(),
            });
        } else if record.coverage() != ResultCoverage::Complete {
            std::sync::Arc::make_mut(&mut record.evidence).coverage = ResultCoverage::Unknown;
        }
        Ok(record)
    }

    /// Finishing before exhaustion preserves uncertainty. An empty exhausted
    /// page is a valid complete empty observation; receiving no page is not.
    pub fn finish(self) -> RecordedCollection<R> {
        let (coverage, reason) = if self.discarded > 0 {
            (ResultCoverage::Partial, Some(Gap::OmittedOccurrences))
        } else {
            match self.termination {
                PageTermination::Exhausted => (ResultCoverage::Complete, None),
                PageTermination::More => (ResultCoverage::Partial, Some(Gap::OmittedOccurrences)),
                PageTermination::Unproven => {
                    (ResultCoverage::Unknown, Some(Gap::UnprovenTermination))
                }
            }
        };
        let gaps: BTreeSet<_> = reason
            .into_iter()
            .map(|reason| EvidenceGap {
                source: self.identity.clone(),
                reason,
            })
            .collect();
        RecordedCollection::from_parts(
            self.identity,
            SharedRows::concat(&self.batches),
            Evidence { coverage, gaps },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection_codec::{CollectionCodec, Demand, RecordingCodec};
    use proptest::prelude::*;

    fn identity(epoch: u64) -> CollectionIdentity {
        CollectionIdentity {
            catalog: [1; 32],
            expression: [2; 32],
            epoch,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        #[test]
        fn acquisition_matches_independent_ordered_sequence(
            pages in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 1..24),
            omitted in 0usize..12,
            exhausted in any::<bool>(),
        ) {
            let id = identity(4);
            let codec = RecordingCodec::<u8>::new();
            let mut acquisition = codec.acquire(id.clone());
            let expected: Vec<_> = pages.iter().flatten().copied().collect();
            let n = pages.len();
            for (i, page) in pages.into_iter().enumerate() {
                let decoded = page.len() + if i == 0 { omitted } else { 0 };
                let termination = if exhausted && i + 1 == n { PageTermination::Exhausted } else { PageTermination::More };
                acquisition.push(&id, i, page.into(), decoded, termination).unwrap();
            }
            let result = acquisition.finish();
            prop_assert_eq!(result.observed(), &expected);
            prop_assert_eq!(result.coverage() == ResultCoverage::Complete, exhausted && omitted == 0);
            prop_assert_eq!(codec.materialize(&result, Demand::Whole).is_ok(), exhausted && omitted == 0);
            let restored = codec.decode(&codec.encode(&result).unwrap(), &id).unwrap();
            prop_assert_eq!(result, restored);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        #[test]
        fn streaming_prefix_requires_an_uninterrupted_prefix(
            first in prop::collection::vec(any::<u8>(), 0..24),
            later in prop::collection::vec(any::<u8>(), 0..24),
            omitted in 0usize..8,
            bound in 0usize..64,
            exhausted in any::<bool>(),
        ) {
            let id = identity(9);
            let codec = RecordingCodec::<u8>::new();
            let mut acquisition = codec.acquire(id.clone());
            let prefix_len = if omitted == 0 { first.len() + later.len() } else { first.len() };
            let expected: Vec<_> = first.iter().chain(&later).copied().take(bound).collect();
            let decoded = first.len() + omitted;
            acquisition.push(&id, 0, first.into(), decoded, PageTermination::More).unwrap();
            let decoded = later.len();
            acquisition.push(&id, 1, later.into(), decoded,
                if exhausted { PageTermination::Exhausted } else { PageTermination::More }).unwrap();
            let result = acquisition.finish_prefix(bound).unwrap();
            let whole = bound <= prefix_len || (omitted == 0 && exhausted);
            prop_assert_eq!(result.observed(), &expected);
            prop_assert_eq!(codec.materialize(&result, Demand::Whole).is_ok(), whole);
            prop_assert_ne!(result.identity(), &id);
        }
    }

    #[test]
    fn empty_page_and_absent_page_are_different_observations() {
        let codec = RecordingCodec::<u8>::new();
        let id = identity(4);
        assert_eq!(
            codec.acquire(id.clone()).finish().coverage(),
            ResultCoverage::Unknown
        );
        let mut acquisition = codec.acquire(id.clone());
        acquisition
            .push(&id, 0, vec![].into(), 0, PageTermination::Exhausted)
            .unwrap();
        assert_eq!(acquisition.finish().coverage(), ResultCoverage::Complete);
    }

    #[test]
    fn invalid_deliveries_are_atomic_and_terminal_pages_are_final() {
        let codec = RecordingCodec::<u8>::new();
        let id = identity(4);
        let mut acquisition = codec.acquire(id.clone());
        for (scope, ordinal, decoded) in
            [(identity(5), 0, 1), (id.clone(), 1, 1), (id.clone(), 0, 0)]
        {
            assert!(acquisition
                .push(
                    &scope,
                    ordinal,
                    vec![7].into(),
                    decoded,
                    PageTermination::More
                )
                .is_err());
        }
        acquisition
            .push(&id, 0, vec![7].into(), 1, PageTermination::More)
            .unwrap();
        assert!(acquisition
            .push(&id, 0, vec![7].into(), 1, PageTermination::More)
            .is_err());
        acquisition
            .push(&id, 1, vec![8].into(), 1, PageTermination::Exhausted)
            .unwrap();
        assert!(acquisition
            .push(&id, 2, vec![9].into(), 1, PageTermination::More)
            .is_err());
        let result = acquisition.finish();
        assert_eq!(result.observed(), &[7, 8]);
        assert_eq!(result.coverage(), ResultCoverage::Complete);
    }

    #[test]
    fn acquisition_shares_payloads_and_preserves_duplicate_occurrences() {
        let id = identity(4);
        let mut acquisition = Acquisition::new(id.clone());
        // Deliberately not Clone, Serialize, or Deserialize: acquisition owns
        // references and never needs to copy or serialize a payload.
        struct Row;
        let batch = SharedRows::from(vec![Row]);
        acquisition
            .push(&id, 0, batch.clone(), 1, PageTermination::More)
            .unwrap();
        acquisition
            .push(&id, 1, batch.clone(), 1, PageTermination::Exhausted)
            .unwrap();
        let result = acquisition.finish();
        assert_eq!(result.observed().len(), 2);
        assert!(std::ptr::eq(&result.observed()[0], &batch[0]));
        assert!(std::ptr::eq(&result.observed()[1], &batch[0]));
    }
}
