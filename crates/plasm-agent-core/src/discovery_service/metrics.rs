//! Low-cardinality discovery stage wall-time metrics.
use opentelemetry::{global, metrics::Histogram, KeyValue};
use std::{sync::OnceLock, time::Duration};

#[derive(Clone, Copy)]
pub(super) enum Stage {
    FrontierLockWait,
    SearchLockWait,
    CatalogueLoad,
    IndexLookupOrBuild,
    Recall,
    DecisionProvider,
    SelectionIncludingProviderAndCheckpoints,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::FrontierLockWait => "frontier_lock_wait",
            Self::SearchLockWait => "search_lock_wait",
            Self::CatalogueLoad => "catalogue_load",
            Self::IndexLookupOrBuild => "index_lookup_or_build",
            Self::Recall => "recall",
            Self::DecisionProvider => "decision_provider",
            Self::SelectionIncludingProviderAndCheckpoints => {
                "selection_including_provider_and_checkpoints"
            }
        }
    }
}

pub(super) fn record(stage: Stage, duration: Duration) {
    static DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    DURATION
        .get_or_init(|| {
            global::meter("plasm")
                .f64_histogram("plasm.discovery.stage.duration_ms")
                .with_description(
                    "Discovery stage wall time; selection includes provider and checkpoint waits.",
                )
                .build()
        })
        .record(
            duration.as_secs_f64() * 1000.0,
            &[KeyValue::new("stage", stage.label())],
        );
}
