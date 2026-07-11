use plasm_core::discovery::RankedCandidate;

use crate::cases::DiscoveryExpect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Ok,
    HardMiss,
    SoftNoise,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CaseMetrics {
    pub hit_at_1_entry: bool,
    pub hit_at_3_entry: bool,
    pub hit_at_1_entity: Option<bool>,
    pub mrr_entry: f64,
    pub noise_at_k: usize,
    pub candidate_count: usize,
    pub failure_class: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseScore {
    pub case_id: String,
    pub intent: String,
    pub baseline_top: Vec<RankedCandidate>,
    pub metrics: CaseMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranked_top: Option<Vec<RankedCandidate>>,
}

pub fn score_lexicon_baseline(
    registry: &plasm_core::discovery::InMemoryCgsRegistry,
    case: &crate::cases::DiscoveryEvalCase,
    k: usize,
) -> CaseScore {
    let baseline =
        crate::baseline::baseline_discover(registry, &case.intent, k).unwrap_or_default();
    CaseScore {
        case_id: case.id.clone(),
        intent: case.intent.clone(),
        baseline_top: baseline.clone(),
        metrics: score_candidates(&case.expect, &baseline, k),
        reranked_top: None,
    }
}

pub fn score_candidates(
    expect: &DiscoveryExpect,
    candidates: &[RankedCandidate],
    k: usize,
) -> CaseMetrics {
    let top_k: Vec<_> = candidates.iter().take(k).collect();
    let allowed: std::collections::HashSet<&str> =
        expect.entry_id_any.iter().map(|s| s.as_str()).collect();

    let hit_at_1_entry = top_k.first().is_some_and(|c| entry_matches(c, expect));
    let hit_at_3_entry = top_k.iter().take(3).any(|c| entry_matches(c, expect));
    let mrr_entry = reciprocal_rank_entry(&top_k, expect);
    let noise_at_k = noise_count(&top_k, &allowed);
    let hit_at_1_entity = if expect.entity_any.is_empty() {
        None
    } else {
        Some(top_k.first().is_some_and(|c| entity_matches(c, expect)))
    };
    let failure_class =
        classify_failure(hit_at_1_entry, hit_at_3_entry, noise_at_k, expect.ambiguous);

    CaseMetrics {
        hit_at_1_entry,
        hit_at_3_entry,
        hit_at_1_entity,
        mrr_entry,
        noise_at_k,
        candidate_count: candidates.len(),
        failure_class: match failure_class {
            FailureClass::Ok => "ok",
            FailureClass::HardMiss => "hard_miss",
            FailureClass::SoftNoise => "soft_noise",
        }
        .to_string(),
    }
}

pub fn classify_failure(
    hit_at_1: bool,
    hit_at_3: bool,
    noise_at_k: usize,
    ambiguous: bool,
) -> FailureClass {
    if hit_at_1 || (ambiguous && hit_at_3) {
        FailureClass::Ok
    } else if !hit_at_3 {
        FailureClass::HardMiss
    } else if noise_at_k >= 4 {
        FailureClass::SoftNoise
    } else {
        FailureClass::Ok
    }
}

fn entry_matches(c: &RankedCandidate, expect: &DiscoveryExpect) -> bool {
    expect.entry_id_any.iter().any(|id| id == &c.entry_id)
}

fn entity_matches(c: &RankedCandidate, expect: &DiscoveryExpect) -> bool {
    if !expect.entity_any.is_empty() && !expect.entity_any.iter().any(|e| e == &c.entity) {
        return false;
    }
    if !expect.capability_name_any.is_empty()
        && !expect
            .capability_name_any
            .iter()
            .any(|n| n == &c.capability_name)
    {
        return false;
    }
    entry_matches(c, expect)
}

fn reciprocal_rank_entry(top: &[&RankedCandidate], expect: &DiscoveryExpect) -> f64 {
    for (i, c) in top.iter().enumerate() {
        if entry_matches(c, expect) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

fn noise_count(top: &[&RankedCandidate], allowed: &std::collections::HashSet<&str>) -> usize {
    top.iter()
        .map(|c| c.entry_id.as_str())
        .filter(|id| !allowed.contains(id))
        .collect::<std::collections::HashSet<_>>()
        .len()
}
