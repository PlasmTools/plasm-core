//! Discovery intent → capability routing eval (MCP lexicon path).

#[cfg(feature = "llm-rerank")]
#[allow(
    clippy::empty_line_after_doc_comments,
    clippy::new_without_default,
    clippy::map_clone,
    clippy::unwrap_or_default,
    clippy::derivable_impls
)]
#[path = "../baml_client/mod.rs"]
pub mod baml_client;

mod baseline;
mod cases;
mod registry;
mod report;
mod score;

#[cfg(feature = "llm-rerank")]
mod rerank;

mod seed_score;
mod seed_trace;

pub use baseline::{baseline_discover, compact_candidates_for_llm};
pub use cases::{
    default_cases_path, default_catalogs_path, load_cases, load_catalog_entry_ids,
    DiscoveryEvalCase, DiscoveryExpect, SeedExpect, SeedRef,
};
pub use registry::{load_registry, resolve_apis_root};
pub use report::{
    build_aggregate, format_human_report, write_human_report, write_json_report, AggregateReport,
    CaseReport,
};
pub use score::{
    classify_failure, score_candidates, score_lexicon_baseline, CaseMetrics, CaseScore,
    FailureClass,
};

#[cfg(feature = "llm-rerank")]
pub use rerank::rerank_candidates;

pub use seed_score::{
    build_seed_aggregate, build_seed_aggregate_from_runs, format_seed_human_report,
    score_seed_selection, score_seed_selector_failure, DecisionBranchMetrics, SeedRunVariance,
    SeedSetAggregateReport, SeedSetCaseScore, SeedSetMetrics,
};

use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::discovery_coverage::{
    coverage_first_selection_raw, coverage_shadow_metrics, postprocess_coverage_selection,
    retrieve_via_coverage,
};
#[cfg(feature = "llm-rerank")]
use plasm_core::discovery_coverage::coverage_route_selection;
use plasm_core::discovery_intent_class::DiscoveryIntentClass;

use seed_trace::{SeedFailureStage, SeedStageTrace};

pub fn case_intents(cases: &[DiscoveryEvalCase]) -> Vec<String> {
    cases.iter().map(|c| c.intent.clone()).collect()
}

pub fn score_all_baseline(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    k: usize,
) -> Vec<CaseScore> {
    cases
        .iter()
        .map(|c| score_lexicon_baseline(registry, c, k))
        .collect()
}

#[cfg(feature = "llm-rerank")]
pub fn score_all_with_rerank(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    k: usize,
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> anyhow::Result<Vec<CaseScore>> {
    let mut out = Vec::with_capacity(cases.len());
    for case in cases {
        let baseline = baseline_discover(registry, &case.intent, k)?;
        let reranked =
            rerank_candidates(&case.intent, &baseline, model, api_key, temperature, seed)?;
        out.push(CaseScore {
            case_id: case.id.clone(),
            intent: case.intent.clone(),
            baseline_top: baseline,
            metrics: score::score_candidates(&case.expect, &reranked, k),
            reranked_top: Some(reranked),
        });
    }
    Ok(out)
}

#[cfg(feature = "llm-rerank")]
pub fn score_all_seed_sets(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> anyhow::Result<Vec<SeedSetCaseScore>> {
    let allowed: Vec<String> = registry.entry_ids();
    score_all_seed_sets_with_allowed(registry, cases, &allowed, model, api_key, temperature, seed)
}

#[cfg(feature = "llm-rerank")]
pub fn score_all_seed_sets_with_allowed(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    allowed_catalogs: &[String],
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> anyhow::Result<Vec<SeedSetCaseScore>> {
    use plasm_core::discovery_seed_baml::build_seed_selection_presentation;
    use plasm_core::discovery_seed_bundle::SeedBundleConfig;
    use plasm_core::discovery_seed_pipeline::prepare_seed_retrieval;
    use plasm_semantic_seed::{
        select_discovery_seeds_detailed, SelectorCatalogHost, SelectorConfig, SelectorRequest,
    };
    use seed_trace::{SeedFailureStage, SeedStageTrace};

    let mut out = Vec::new();
    for case in cases {
        let Some(expect) = case.seed_expect.as_ref() else {
            continue;
        };
        let mut trace = SeedStageTrace::default();
        let prep = match prepare_seed_retrieval(registry, &case.intent, allowed_catalogs) {
            Ok(prep) => prep,
            Err(error) => {
                trace.record_failure(SeedFailureStage::Retrieve, error.to_string());
                out.push(score_seed_selector_failure(
                    &case.id,
                    &case.intent,
                    &case.tags,
                    &error.to_string(),
                    Some(trace),
                ));
                continue;
            }
        };
        trace.record_retrieval_policy(&prep.intent_class, &prep.named_catalogs);

        let retrieved = match retrieve_via_coverage(
            registry,
            &case.intent,
            Some(allowed_catalogs),
            &prep.intent_class,
            &prep.named_catalogs,
        ) {
            Ok(retrieved) => retrieved,
            Err(error) => {
                trace.record_failure(SeedFailureStage::Retrieve, error.to_string());
                out.push(score_seed_selector_failure(
                    &case.id,
                    &case.intent,
                    &case.tags,
                    &error.to_string(),
                    Some(trace),
                ));
                continue;
            }
        };
        let presentation = build_seed_selection_presentation(
            &retrieved.bundles,
            SeedBundleConfig::default(),
            retrieved.catalog_context.clone(),
            &retrieved.named_catalogs,
            retrieved.candidate_graph.clone(),
        )
        .ok()
        .flatten();
        trace.presented_symbols = presentation.as_ref().map(|presentation| {
            presentation
                .symbol_map
                .rows()
                .iter()
                .map(|row| row.symbol.clone())
                .collect()
        });
        trace.record_retrieve(
            &retrieved.bundles,
            presentation
                .as_ref()
                .map(|presentation| presentation.symbol_map.symbol_count())
                .unwrap_or(0),
        );
        trace.gold_in_pool = Some(SeedStageTrace::gold_in_pool(
            &retrieved.bundles,
            &expect.acceptable_sets,
        ));
        let acceptable_gold: Vec<Vec<(String, String)>> = expect
            .acceptable_sets
            .iter()
            .map(|set| {
                set.iter()
                    .map(|seed| (seed.entry_id.clone(), seed.entity.clone()))
                    .collect()
            })
            .collect();
        let coverage_pipeline = coverage_route_selection(registry, &case.intent, allowed_catalogs)
            .ok()
            .map(|(pipeline, _)| pipeline);
        let detailed = match select_discovery_seeds_detailed(
            SelectorRequest {
                intent: &case.intent,
                intent_class: &retrieved.intent_class,
                bundles: &retrieved.bundles,
                catalog_context: &retrieved.catalog_context,
                brand_lock_catalogs: &retrieved.named_catalogs,
                candidate_graph: retrieved.candidate_graph.clone(),
            },
            SelectorConfig {
                client_name: "EvalModel",
                model,
                api_key,
                temperature,
                seed,
            },
            Some(SelectorCatalogHost {
                catalog: registry,
                allowed_entry_ids: allowed_catalogs,
            }),
        ) {
            Ok(detailed) => detailed,
            Err(error) => {
                trace.record_failure(SeedFailureStage::Select, error.to_string());
                if let Some(ref pipeline) = coverage_pipeline {
                    trace.coverage =
                        Some(coverage_shadow_metrics(pipeline, &acceptable_gold, None));
                }
                out.push(score_seed_selector_failure(
                    &case.id,
                    &case.intent,
                    &case.tags,
                    &error.to_string(),
                    Some(trace),
                ));
                continue;
            }
        };
        trace.coverage = Some(coverage_shadow_metrics(
            &detailed.pipeline,
            &acceptable_gold,
            Some(&detailed.selection),
        ));
        let raw = detailed.selection;
        trace.record_select_raw(&raw, &[]);
        out.push(score_seed_selection(
            &case.id,
            &case.intent,
            &case.tags,
            expect,
            &retrieved.bundles,
            raw,
            Some(trace),
        ));
    }
    Ok(out)
}

/// Coverage pipeline shadow eval (no LLM): route + select from deterministic coverage only.
pub fn score_all_coverage_shadow(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    allowed_catalogs: &[String],
) -> anyhow::Result<Vec<SeedSetCaseScore>> {
    score_all_coverage_shadow_with_margin(
        registry,
        cases,
        allowed_catalogs,
        plasm_core::discovery_coverage::READY_MARGIN,
    )
}

/// Shadow scoring with an explicit READY_MARGIN (holdout risk-coverage sweep).
pub fn score_all_coverage_shadow_with_margin(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    allowed_catalogs: &[String],
    margin: u32,
) -> anyhow::Result<Vec<SeedSetCaseScore>> {
    use plasm_core::discovery_coverage::coverage_route_selection_with_margin;
    use plasm_core::discovery_seed_select::{SeedSelectionDecision, SeedSelectionRaw};

    let mut out = Vec::new();
    for case in cases {
        let Some(expect) = case.seed_expect.as_ref() else {
            continue;
        };
        let mut trace = SeedStageTrace::default();
        let retrieved = match retrieve_via_coverage(
            registry,
            &case.intent,
            Some(allowed_catalogs),
            &DiscoveryIntentClass::ReadListNav,
            &[],
        ) {
            Ok(retrieved) => retrieved,
            Err(error) => {
                trace.record_failure(SeedFailureStage::Retrieve, error.to_string());
                out.push(score_seed_selector_failure(
                    &case.id,
                    &case.intent,
                    &case.tags,
                    &error.to_string(),
                    Some(trace),
                ));
                continue;
            }
        };
        let acceptable_gold: Vec<Vec<(String, String)>> = expect
            .acceptable_sets
            .iter()
            .map(|set| {
                set.iter()
                    .map(|seed| (seed.entry_id.clone(), seed.entity.clone()))
                    .collect()
            })
            .collect();
        let raw = match coverage_route_selection_with_margin(
            registry,
            &case.intent,
            allowed_catalogs,
            margin,
        ) {
            Ok((pipeline, _)) => {
                let raw = coverage_first_selection_raw(&pipeline).unwrap_or(SeedSelectionRaw {
                    decision: SeedSelectionDecision::HardMiss,
                    requirements: Vec::new(),
                    selected_ids: Vec::new(),
                    supporting_capability_ids: Vec::new(),
                    alternative_sets: Vec::new(),
                    uncovered_requirements: Vec::new(),
                    reasoning: "coverage shadow: no selection".into(),
                });
                let raw = postprocess_coverage_selection(
                    raw,
                    &retrieved.bundles,
                    &retrieved.intent_class,
                    &retrieved.catalog_context,
                    &retrieved.candidate_graph,
                    Some(&pipeline.route),
                    Some(&pipeline.evaluation),
                );
                trace.coverage = Some(coverage_shadow_metrics(
                    &pipeline,
                    &acceptable_gold,
                    Some(&raw),
                ));
                raw
            }
            Err(error) => {
                trace.record_failure(SeedFailureStage::Retrieve, error.to_string());
                SeedSelectionRaw {
                    decision: SeedSelectionDecision::HardMiss,
                    requirements: Vec::new(),
                    selected_ids: Vec::new(),
                    supporting_capability_ids: Vec::new(),
                    alternative_sets: Vec::new(),
                    uncovered_requirements: vec![error.to_string()],
                    reasoning: error.to_string(),
                }
            }
        };
        out.push(score_seed_selection(
            &case.id,
            &case.intent,
            &case.tags,
            expect,
            &retrieved.bundles,
            raw,
            Some(trace),
        ));
    }
    Ok(out)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadyMarginSweepPoint {
    pub margin: u32,
    pub ready_coverage: f64,
    pub false_open_at_coverage: f64,
    pub false_open_rate: f64,
    pub decision_accuracy: f64,
    pub hard_miss_to_ready_count: usize,
    pub plan_select_exact_rate: f64,
}

/// Sweep READY_MARGIN on a case set (typically holdout). Does not mutate the const.
pub fn sweep_ready_margins(
    registry: &InMemoryCgsRegistry,
    cases: &[DiscoveryEvalCase],
    allowed_catalogs: &[String],
    margins: &[u32],
) -> anyhow::Result<Vec<ReadyMarginSweepPoint>> {
    let mut out = Vec::with_capacity(margins.len());
    for &margin in margins {
        let scores =
            score_all_coverage_shadow_with_margin(registry, cases, allowed_catalogs, margin)?;
        let report = build_seed_aggregate(&scores);
        out.push(ReadyMarginSweepPoint {
            margin,
            ready_coverage: report.ready_coverage,
            false_open_at_coverage: report.false_open_at_coverage,
            false_open_rate: report.false_open_rate,
            decision_accuracy: report.decision_accuracy,
            hard_miss_to_ready_count: report.hard_miss_to_ready_count,
            plan_select_exact_rate: report.plan_select_exact_rate,
        });
    }
    Ok(out)
}

pub type RankedCandidate = plasm_core::RankedCandidate;
