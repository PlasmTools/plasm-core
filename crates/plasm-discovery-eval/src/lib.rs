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
#[cfg(feature = "llm-rerank")]
mod seed_select;

mod seed_score;

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
#[cfg(feature = "llm-rerank")]
pub use seed_select::select_discovery_seeds;

pub use seed_score::{
    build_seed_aggregate, format_seed_human_report, score_seed_selection, SeedSetAggregateReport,
    SeedSetCaseScore, SeedSetMetrics,
};

use plasm_core::discovery::InMemoryCgsRegistry;

#[cfg(feature = "llm-rerank")]
use plasm_core::discovery_auto_seed::{retrieve_entity_candidate_bundles, EntityCandidateConfig};

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
    let mut out = Vec::new();
    for case in cases {
        let Some(expect) = case.seed_expect.as_ref() else {
            continue;
        };
        let bundles = retrieve_entity_candidate_bundles(
            registry,
            &case.intent,
            None,
            EntityCandidateConfig::default(),
        )?;
        let raw =
            match select_discovery_seeds(&case.intent, &bundles, model, api_key, temperature, seed)
            {
                Ok(raw) => raw,
                Err(e) => {
                    out.push(seed_score::score_seed_selector_failure(
                        &case.id,
                        &case.intent,
                        &e.to_string(),
                    ));
                    continue;
                }
            };
        out.push(score_seed_selection(
            &case.id,
            &case.intent,
            expect,
            &bundles,
            raw,
        ));
    }
    Ok(out)
}

pub type RankedCandidate = plasm_core::RankedCandidate;
