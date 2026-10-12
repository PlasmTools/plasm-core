//! Source-independent row observation. Storage handles and transport profiles are
//! intentionally absent: they cannot change which evidence the renderer exposes.
use super::InBandSummaryReport;
use plasm_runtime::{CachedEntity, ExecutionResult, ResultCoverage};

pub(crate) trait RowObservation {
    fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity>;
    fn coverage(&self) -> ResultCoverage;
}

impl RowObservation for ExecutionResult {
    fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity> {
        self.entities().iter()
    }
    fn coverage(&self) -> ResultCoverage {
        self.coverage()
    }
}

pub(crate) struct RenderedObservation {
    pub tsv: String,
    pub shown: usize,
    pub coverage: ResultCoverage,
    pub fidelity: InBandSummaryReport,
}

/// All row sources use this renderer. At least one row survives any display budget;
/// field previews are explicitly marked and cannot be mistaken for exact values.
pub(crate) fn render_observation(
    source: &impl RowObservation,
    cgs: Option<&plasm_core::CGS>,
    max_rows: usize,
    target_bytes: usize,
) -> RenderedObservation {
    let mut shown = source.rows().len().min(max_rows.max(1));
    loop {
        let (tsv, fidelity) = super::summary::format_result_tsv_observation(source, cgs, shown);
        if tsv.len() <= target_bytes || shown <= 1 {
            return RenderedObservation {
                tsv,
                shown,
                coverage: source.coverage(),
                fidelity,
            };
        }
        shown = (shown / 2).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    struct DetachedRows(Vec<CachedEntity>);
    impl RowObservation for DetachedRows {
        fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity> {
            self.0.iter()
        }
        fn coverage(&self) -> ResultCoverage {
            ResultCoverage::Unknown
        }
    }
    #[test]
    fn detached_rows_and_execution_results_share_the_renderer() {
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(50, None);
        let detached = DetachedRows(step.result.entities().iter().cloned().collect());
        let live = render_observation(step.result.as_ref(), None, 25, 4000);
        let other = render_observation(&detached, None, 25, 4000);
        assert_eq!(live.tsv, other.tsv);
        assert_eq!(live.shown, other.shown);
        assert_eq!(live.coverage, other.coverage);
        assert_eq!(live.shown, 25);
    }
    #[test]
    fn exhausted_display_budget_preserves_a_row_but_not_a_false_empty_result() {
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(50, None);
        let rendered = render_observation(step.result.as_ref(), None, 0, 0);
        assert_eq!(rendered.shown, 1);
        assert_eq!(rendered.tsv.lines().count(), 2);
        let empty = DetachedRows(vec![]);
        assert_eq!(render_observation(&empty, None, 0, 0).shown, 0);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn rowset_properties_source_independent_rendering_is_bounded_and_non_destructive(
            count in 0usize..300, max_rows in 0usize..300, budget in 0usize..16000,
            text in ".{0,1500}",
        ) {
            let step = crate::test_support::execution_fixtures::synthetic_published_result_step(count, None);
            let mut rows: Vec<_> = step.result.entities().iter().cloned().collect();
            for (i, row) in rows.iter_mut().enumerate() {
                row.fields.insert("computed_alias".into(), plasm_core::Value::String(format!("{i}:{text}")).into());
            }
            let source = DetachedRows(rows.clone());
            let mut live = step.result.as_ref().clone();
            live.collection = crate::test_support::execution_fixtures::collection(rows.clone(), ResultCoverage::Unknown);
            let a = render_observation(&source, None, max_rows, budget);
            let b = render_observation(&live, None, max_rows, budget);
            prop_assert_eq!(&a.tsv, &b.tsv);
            prop_assert_eq!(a.shown, b.shown);
            prop_assert!(a.shown <= count.min(max_rows.max(1)));
            prop_assert!(a.tsv.len() <= budget || a.shown <= 1);
            prop_assert_eq!(source.rows().len(), count);
            prop_assert_eq!(live.entities().len(), count);
            if count != 0 {
                prop_assert!(a.tsv.lines().next().unwrap().contains("computed_alias"));
                prop_assert!(a.shown >= 1);
            }
            // Increasing the context budget must never discard additional rows.
            let larger = render_observation(&source, None, max_rows, budget.saturating_mul(2));
            prop_assert!(larger.shown >= a.shown);
        }
    }
}
