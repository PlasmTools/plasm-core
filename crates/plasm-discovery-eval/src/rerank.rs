use anyhow::Context;
use plasm_core::discovery::RankedCandidate;
use plasm_eval_common::openrouter_eval_llm_options;

use crate::baml_client::sync_client::B;
use crate::baml_client::types::{DiscoveryRerankCandidate, RerankedDiscovery};
use crate::baml_client::ClientRegistry;

#[cfg(feature = "llm-rerank")]
pub fn rerank_candidates(
    intent: &str,
    baseline: &[RankedCandidate],
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> anyhow::Result<Vec<RankedCandidate>> {
    if baseline.is_empty() {
        return Ok(Vec::new());
    }
    crate::baml_client::init();

    let candidates: Vec<DiscoveryRerankCandidate> = baseline
        .iter()
        .enumerate()
        .map(|(i, c)| DiscoveryRerankCandidate {
            rank: i as i64,
            entry_id: c.entry_id.clone(),
            entity: c.entity.clone(),
            capability_name: c.capability_name.clone(),
            description: c.capability_description.clone(),
        })
        .collect();

    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(model, api_key, temperature, seed),
    );
    registry.set_primary_client("EvalModel");

    let mut last_err = None;
    for attempt in 0..3u32 {
        match B
            .RerankDiscoveryCandidates
            .with_client_registry(&registry)
            .call(intent, candidates.as_slice())
        {
            Ok(out) => return apply_rerank_order(baseline, &out),
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    eprintln!(
                        "rerank retry {}/2 for intent: {}",
                        attempt + 1,
                        trunc_intent(intent, 48)
                    );
                }
            }
        }
    }
    Err(last_err
        .map(|e| anyhow::anyhow!("BAML RerankDiscoveryCandidates: {e}"))
        .unwrap_or_else(|| anyhow::anyhow!("BAML RerankDiscoveryCandidates failed")))
    .context("LLM rerank")
}

fn trunc_intent(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(feature = "llm-rerank")]
fn apply_rerank_order(
    baseline: &[RankedCandidate],
    out: &RerankedDiscovery,
) -> anyhow::Result<Vec<RankedCandidate>> {
    let mut seen = std::collections::HashSet::new();
    let mut reranked = Vec::with_capacity(baseline.len());
    for idx in &out.ordered_ids {
        let i = *idx as usize;
        if i >= baseline.len() {
            continue;
        }
        if seen.insert(i) {
            reranked.push(baseline[i].clone());
        }
    }
    for (i, c) in baseline.iter().enumerate() {
        if seen.insert(i) {
            reranked.push(c.clone());
        }
    }
    Ok(reranked)
}

#[cfg(not(feature = "llm-rerank"))]
pub fn rerank_candidates(
    _intent: &str,
    _baseline: &[RankedCandidate],
    _model: &str,
    _api_key: &str,
    _temperature: f64,
    _seed: u64,
) -> anyhow::Result<Vec<RankedCandidate>> {
    anyhow::bail!("rebuild with --features llm-rerank")
}
