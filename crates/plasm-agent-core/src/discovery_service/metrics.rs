//! Low-cardinality discovery stage wall-time metrics.
use opentelemetry::{
    global,
    metrics::{Counter, Histogram},
    KeyValue,
};
use std::{sync::OnceLock, time::Duration};

const EXTRACTION_TOTAL: &str = "plasm.discovery.operation_extraction.total";

#[derive(Clone, Copy)]
pub(super) enum Stage {
    FrontierLockWait,
    SearchLockWait,
    CatalogueLoad,
    IndexLookupOrBuild,
    Recall,
    OperationExtraction,
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
            Self::OperationExtraction => "operation_extraction",
            Self::DecisionProvider => "decision_provider",
            Self::SelectionIncludingProviderAndCheckpoints => {
                "selection_including_provider_and_checkpoints"
            }
        }
    }
}

pub(super) fn record_extraction(duration: Duration, success: bool) {
    static TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    record(Stage::OperationExtraction, duration);
    TOTAL
        .get_or_init(|| {
            global::meter("plasm")
                .u64_counter(EXTRACTION_TOTAL)
                .with_description("Operation extraction requests by outcome.")
                .build()
        })
        .add(
            1,
            &[KeyValue::new(
                "result",
                if success { "success" } else { "error" },
            )],
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_extraction_uses_stable_low_cardinality_metric_names() {
        assert_eq!(Stage::OperationExtraction.label(), "operation_extraction");
        assert_eq!(
            EXTRACTION_TOTAL,
            "plasm.discovery.operation_extraction.total"
        );
    }
}
