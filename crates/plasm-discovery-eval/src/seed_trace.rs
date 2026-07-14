//! Per-case stage traces for seed-set eval failure attribution.

use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_coverage::CoverageShadowMetrics;
use plasm_core::discovery_intent_class::DiscoveryIntentClass;
use plasm_core::discovery_seed_select::{SeedSelectionDecision, SeedSelectionRaw};
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedFailureStage {
    Retrieve,
    Select,
    Resolve,
    Validate,
    Score,
}

/// Snapshot of one eval pipeline stage.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SeedStageTrace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_catalogs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presented_symbols: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_selected_symbols: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_candidate_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<SeedFailureStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gold_in_pool: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageShadowMetrics>,
}

impl SeedStageTrace {
    pub fn bundle_keys(bundles: &[EntityCandidateBundle]) -> Vec<String> {
        bundles
            .iter()
            .map(|b| format!("{}:{}", b.entry_id, b.entity))
            .collect()
    }

    pub fn gold_in_pool(
        bundles: &[EntityCandidateBundle],
        acceptable: &[Vec<crate::cases::SeedRef>],
    ) -> bool {
        if acceptable.is_empty() {
            return true;
        }
        let pool: std::collections::HashSet<_> = bundles
            .iter()
            .map(|b| (b.entry_id.as_str(), b.entity.as_str()))
            .collect();
        acceptable.iter().any(|gold| {
            gold.iter()
                .all(|s| pool.contains(&(s.entry_id.as_str(), s.entity.as_str())))
        })
    }

    pub fn record_retrieval_policy(
        &mut self,
        intent_class: &DiscoveryIntentClass,
        named_catalogs: &[String],
    ) {
        self.intent_class = Some(intent_class.wire_name().to_string());
        self.named_catalogs = Some(named_catalogs.to_vec());
    }

    pub fn record_retrieve(&mut self, bundles: &[EntityCandidateBundle], symbol_count: usize) {
        self.bundle_count = Some(bundles.len());
        self.symbol_count = Some(symbol_count);
    }

    pub fn record_select_raw(&mut self, raw: &SeedSelectionRaw, symbols: &[String]) {
        self.raw_decision = Some(decision_wire(raw.decision));
        self.raw_selected_symbols = Some(symbols.to_vec());
        self.resolved_candidate_ids = Some(raw.selected_ids.clone());
    }

    pub fn record_failure(&mut self, stage: SeedFailureStage, detail: impl Into<String>) {
        self.failure_stage = Some(stage);
        self.failure_detail = Some(detail.into());
    }
}

fn decision_wire(d: SeedSelectionDecision) -> String {
    match d {
        SeedSelectionDecision::Ready => "ready".into(),
        SeedSelectionDecision::Clarify => "clarify".into(),
        SeedSelectionDecision::HardMiss => "hard_miss".into(),
    }
}
