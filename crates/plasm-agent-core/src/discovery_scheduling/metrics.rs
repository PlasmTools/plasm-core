//! Provider admission and service timings with bounded attributes.
use opentelemetry::{
    global,
    metrics::{Counter, Histogram},
    KeyValue,
};
use std::{sync::OnceLock, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Workload {
    OperationExtraction,
    DiscoveryDecision,
    LearningNormalization,
    LearningApplicability,
    LearningEmbedding,
}

impl Workload {
    fn label(self) -> &'static str {
        match self {
            Self::OperationExtraction => "operation_extraction",
            Self::DiscoveryDecision => "discovery_decision",
            Self::LearningNormalization => "learning_normalization",
            Self::LearningApplicability => "learning_applicability",
            Self::LearningEmbedding => "learning_embedding",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    PermitWait,
    ProviderService,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::PermitWait => "permit_wait",
            Self::ProviderService => "provider_service",
        }
    }
}

/// Records cancellation on drop, including cancellation while waiting for admission.
pub(crate) struct PhaseTimer {
    operation: Workload,
    phase: Phase,
    started: Instant,
    result: &'static str,
}

impl PhaseTimer {
    pub(crate) fn start(operation: Workload, phase: Phase) -> Self {
        Self {
            operation,
            phase,
            started: Instant::now(),
            result: "cancelled",
        }
    }

    pub(crate) fn complete(mut self, success: bool) {
        self.result = if success { "success" } else { "error" };
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        static DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
        static TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
        let attrs = [
            KeyValue::new("operation", self.operation.label()),
            KeyValue::new("phase", self.phase.label()),
            KeyValue::new("result", self.result),
        ];
        let duration_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        DURATION
            .get_or_init(|| {
                global::meter("plasm")
                    .f64_histogram("plasm.discovery.provider.phase.duration_ms")
                    .with_description(
                        "Provider permit wait or service duration, including cancelled attempts.",
                    )
                    .build()
            })
            .record(duration_ms, &attrs);
        TOTAL
            .get_or_init(|| {
                global::meter("plasm")
                    .u64_counter("plasm.discovery.provider.phase.total")
                    .with_description("Provider permit wait and service phases by outcome.")
                    .build()
            })
            .add(1, &attrs);
        tracing::debug!(
            operation = self.operation.label(),
            phase = self.phase.label(),
            result = self.result,
            duration_ms,
            "discovery provider phase completed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_are_closed_and_distinguish_workloads_and_phases() {
        let operations = [
            Workload::OperationExtraction,
            Workload::DiscoveryDecision,
            Workload::LearningNormalization,
            Workload::LearningApplicability,
            Workload::LearningEmbedding,
        ];
        let labels: std::collections::BTreeSet<_> =
            operations.into_iter().map(Workload::label).collect();
        assert_eq!(labels.len(), 5);
        assert_ne!(Phase::PermitWait.label(), Phase::ProviderService.label());
    }
}
