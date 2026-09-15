//! Result coverage relative to the requested expression — not the archive.
//!
//! An artifact is a complete *stored snapshot* of the rows supplied to it. Those rows may
//! already be bounded. [`ResultCoverage`] says whether that snapshot represents the complete
//! expression result.
//!
//! Existing snapshots without `coverage` deserialize as [`ResultCoverage::Unknown`]. That is
//! the honest default for documents that predate this field — not an optional dual path.

use serde::{Deserialize, Serialize};

use super::types::{
    ConsumeBoundKind, ExecutionStats, PageResult, QueryPaginationResumeData, StreamConsumeOpts,
};
use crate::CachedEntity;

/// Whether a represented result fully covers the requested expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResultCoverage {
    /// Execution established the expression’s result is fully represented.
    Complete,
    /// Execution knows rows remain outside the represented result.
    Partial,
    /// Execution cannot establish completeness.
    ///
    /// Deserialize default for stored documents that omit `coverage`.
    #[default]
    Unknown,
}

impl ResultCoverage {
    /// Stable wire token (`complete` / `partial` / `unknown`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }

    /// Combine independently produced coverages. Partial dominates; Complete only if every
    /// operand is Complete. Do not flatten independently bounded reads into Complete.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Partial, _) | (_, Self::Partial) => Self::Partial,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }

    /// Fold coverages from several results. Empty iterator is Unknown (no proof).
    #[must_use]
    pub fn combine_all(iter: impl IntoIterator<Item = Self>) -> Self {
        let mut acc: Option<Self> = None;
        for next in iter {
            acc = Some(match acc {
                Some(prev) => prev.combine(next),
                None => next,
            });
        }
        acc.unwrap_or(Self::Unknown)
    }
}

impl std::fmt::Display for ResultCoverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a consume loop stopped. Used to populate [`ResultCoverage`] and presentation `has_more`
/// without guessing Complete or hand-syncing the two fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeStop {
    /// Pagination driver established the backend has no further pages.
    BackendExhausted,
    /// Pagination driver established further pages exist.
    BackendHasMore,
    /// `max_items` stopped the loop.
    ///
    /// `truncated_page` when this HTTP page held extra rows beyond the keep budget.
    /// `driver_has_more` folds the pagination-driver answer when consulted (`None` when not).
    ItemCapHit {
        truncated_page: bool,
        driver_has_more: Option<bool>,
    },
    /// Filter/limit row-match budget was satisfied.
    RowMatchBudgetSatisfied,
    /// Empty page observed without a driver completeness signal.
    EmptyPageUnproven,
    /// Cache/replay consult without a stored completeness proof.
    CacheWithoutProof,
}

impl ConsumeStop {
    /// Presentation paging: another poll may return more rows for the same query/stream.
    ///
    /// Distinct from expression incompleteness ([`ResultCoverage::Partial`]): truncation can be
    /// Partial with `has_more == false` when the driver was not consulted.
    #[must_use]
    pub fn has_more(self) -> bool {
        match self {
            Self::BackendHasMore => true,
            Self::ItemCapHit {
                driver_has_more: Some(true),
                ..
            } => true,
            _ => false,
        }
    }
}

/// Coverage for a consume termination. Host caps cannot be Complete merely because
/// execution succeeded. Explicit expression `take` that is satisfied is Complete.
///
/// Contract:
/// - **Partial** = demonstrated truncation (`truncated_page`) or positive continuation
///   (`BackendHasMore` / `ItemCapHit { driver_has_more: Some(true), .. }`).
/// - Exactly-full host page without driver evidence → **Unknown** (not Partial).
/// - Truncation always Partial for host bounds — driver exhaustion must not promote discarded
///   rows to Complete.
/// - Missing deserialize → **Unknown**.
#[must_use]
pub fn coverage_for_consume_stop(opts: &StreamConsumeOpts, stop: ConsumeStop) -> ResultCoverage {
    match stop {
        ConsumeStop::BackendExhausted => ResultCoverage::Complete,
        ConsumeStop::BackendHasMore => ResultCoverage::Partial,
        ConsumeStop::ItemCapHit {
            truncated_page,
            driver_has_more,
        } => match opts.bound_kind {
            ConsumeBoundKind::ExpressionTake => ResultCoverage::Complete,
            ConsumeBoundKind::HostPage | ConsumeBoundKind::None => {
                if truncated_page {
                    ResultCoverage::Partial
                } else {
                    // Exactly-full page at the host cap: only a driver answer decides.
                    match driver_has_more {
                        Some(true) => ResultCoverage::Partial,
                        Some(false) => ResultCoverage::Complete,
                        None => ResultCoverage::Unknown,
                    }
                }
            }
        },
        ConsumeStop::RowMatchBudgetSatisfied => match opts.bound_kind {
            ConsumeBoundKind::ExpressionTake => ResultCoverage::Complete,
            ConsumeBoundKind::HostPage | ConsumeBoundKind::None => ResultCoverage::Partial,
        },
        ConsumeStop::EmptyPageUnproven | ConsumeStop::CacheWithoutProof => ResultCoverage::Unknown,
    }
}

/// One stream page: stamps `has_more` and `coverage` from the same [`ConsumeStop`].
///
/// Call sites must not hand-assign the two fields from parallel bool soups.
#[must_use]
pub fn page_result(
    entities: Vec<CachedEntity>,
    page_index: usize,
    opts: &StreamConsumeOpts,
    stop: ConsumeStop,
    pagination_resume: Option<QueryPaginationResumeData>,
    stats: ExecutionStats,
) -> PageResult {
    PageResult {
        entities,
        page_index,
        has_more: stop.has_more(),
        coverage: coverage_for_consume_stop(opts, stop),
        pagination_resume,
        stats,
        operations: super::OperationLedger::empty(),
    }
}

/// Coverage after an explicit take over a source collection.
///
/// Satisfied take (`got >= take`) is Complete even when the source was Partial.
/// An unsatisfied take inherits the source: only a Complete source proves those
/// fewer rows are the full expression result.
#[must_use]
pub fn coverage_after_explicit_take(
    source: ResultCoverage,
    take: usize,
    got: usize,
) -> ResultCoverage {
    if got >= take {
        ResultCoverage::Complete
    } else {
        source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::types::StreamConsumeOpts;

    fn opts(kind: ConsumeBoundKind) -> StreamConsumeOpts {
        StreamConsumeOpts {
            bound_kind: kind,
            ..Default::default()
        }
    }

    #[test]
    fn host_item_cap_truncated_page_is_partial() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::HostPage),
            ConsumeStop::ItemCapHit {
                truncated_page: true,
                driver_has_more: None,
            },
        );
        assert_eq!(c, ResultCoverage::Partial);
    }

    #[test]
    fn host_item_cap_exactly_full_without_driver_is_unknown() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::HostPage),
            ConsumeStop::ItemCapHit {
                truncated_page: false,
                driver_has_more: None,
            },
        );
        assert_eq!(c, ResultCoverage::Unknown);
    }

    #[test]
    fn explicit_take_item_cap_is_complete() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::ExpressionTake),
            ConsumeStop::ItemCapHit {
                truncated_page: true,
                driver_has_more: Some(true),
            },
        );
        assert_eq!(c, ResultCoverage::Complete);
    }

    #[test]
    fn backend_exhausted_is_complete() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::HostPage),
            ConsumeStop::BackendExhausted,
        );
        assert_eq!(c, ResultCoverage::Complete);
    }

    #[test]
    fn empty_unproven_is_unknown() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::None),
            ConsumeStop::EmptyPageUnproven,
        );
        assert_eq!(c, ResultCoverage::Unknown);
        let empty_ok =
            coverage_for_consume_stop(&opts(ConsumeBoundKind::None), ConsumeStop::BackendExhausted);
        assert_eq!(empty_ok, ResultCoverage::Complete);
    }

    #[test]
    fn cache_without_proof_is_unknown() {
        let c = coverage_for_consume_stop(
            &opts(ConsumeBoundKind::None),
            ConsumeStop::CacheWithoutProof,
        );
        assert_eq!(c, ResultCoverage::Unknown);
    }

    #[test]
    fn combine_does_not_promote_partial_fanout_to_complete() {
        assert_eq!(
            ResultCoverage::combine_all([
                ResultCoverage::Complete,
                ResultCoverage::Partial,
                ResultCoverage::Complete
            ]),
            ResultCoverage::Partial
        );
        assert_eq!(
            ResultCoverage::combine_all([ResultCoverage::Complete, ResultCoverage::Unknown]),
            ResultCoverage::Unknown
        );
        assert_eq!(ResultCoverage::combine_all([]), ResultCoverage::Unknown);
    }

    #[test]
    fn take_satisfied_is_complete_over_partial_source() {
        assert_eq!(
            coverage_after_explicit_take(ResultCoverage::Partial, 5, 5),
            ResultCoverage::Complete
        );
        assert_eq!(
            coverage_after_explicit_take(ResultCoverage::Partial, 5, 3),
            ResultCoverage::Partial
        );
        assert_eq!(
            coverage_after_explicit_take(ResultCoverage::Complete, 5, 3),
            ResultCoverage::Complete
        );
        assert_eq!(
            coverage_after_explicit_take(ResultCoverage::Unknown, 5, 0),
            ResultCoverage::Unknown
        );
    }

    #[test]
    fn exactly_full_host_page_without_driver_is_unknown() {
        let stop = ConsumeStop::ItemCapHit {
            truncated_page: false,
            driver_has_more: None,
        };
        assert_eq!(
            coverage_for_consume_stop(&opts(ConsumeBoundKind::HostPage), stop),
            ResultCoverage::Unknown
        );
        assert!(!stop.has_more());
    }

    #[test]
    fn host_page_exhausted_at_cap_without_discard_is_complete() {
        let stop = ConsumeStop::ItemCapHit {
            truncated_page: false,
            driver_has_more: Some(false),
        };
        assert_eq!(
            coverage_for_consume_stop(&opts(ConsumeBoundKind::HostPage), stop),
            ResultCoverage::Complete
        );
        assert!(!stop.has_more());
    }

    #[test]
    fn final_page_truncation_with_exhausted_driver_is_partial() {
        // Final backend page held 40 rows, host kept 25, driver reports no next page.
        // Exhaustion must not promote discarded rows to Complete.
        let stop = ConsumeStop::ItemCapHit {
            truncated_page: true,
            driver_has_more: Some(false),
        };
        let c = coverage_for_consume_stop(&opts(ConsumeBoundKind::HostPage), stop);
        assert_eq!(c, ResultCoverage::Partial);
        assert_ne!(c, ResultCoverage::Complete);
        assert!(!stop.has_more());
    }

    #[test]
    fn truncation_with_driver_has_more_stamps_both_fields() {
        let stop = ConsumeStop::ItemCapHit {
            truncated_page: true,
            driver_has_more: Some(true),
        };
        let page = page_result(
            Vec::new(),
            0,
            &opts(ConsumeBoundKind::HostPage),
            stop,
            None,
            ExecutionStats::default(),
        );
        assert!(page.has_more);
        assert_eq!(page.coverage, ResultCoverage::Partial);
    }

    #[test]
    fn deserialize_default_is_unknown() {
        let v: ResultCoverage = serde_json::from_value(serde_json::Value::Null)
            .ok()
            .unwrap_or_default();
        assert_eq!(v, ResultCoverage::Unknown);
        let from_missing: ResultCoverage = serde_json::from_str("null").unwrap_or_default();
        assert_eq!(from_missing, ResultCoverage::Unknown);
    }
}
