//! Seed-set eval scoring (`seed_expect` in cases.yaml).

use std::collections::HashSet;

use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_seed_select::{
    validate_seed_selection, SeedSelectionDecision, SeedSelectionRaw, ValidatedSeedSelection,
};

use crate::cases::{SeedExpect, SeedRef};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SeedSetMetrics {
    pub decision: String,
    pub decision_ok: bool,
    pub bundle_exact: bool,
    pub seed_precision: f64,
    pub seed_recall: f64,
    pub false_open: bool,
    pub excluded_violation: bool,
    pub routing_error: bool,
    pub seed_count: usize,
    pub supporting_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedSetCaseScore {
    pub case_id: String,
    pub intent: String,
    pub metrics: SeedSetMetrics,
    pub selected_seeds: Vec<SeedRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

pub fn score_seed_selection(
    case_id: &str,
    intent: &str,
    expect: &SeedExpect,
    bundles: &[EntityCandidateBundle],
    raw: SeedSelectionRaw,
) -> SeedSetCaseScore {
    let reasoning = if raw.reasoning.is_empty() {
        None
    } else {
        Some(raw.reasoning.clone())
    };
    match validate_seed_selection(&raw, bundles) {
        Ok(ValidatedSeedSelection::Ready(ready)) => {
            let selected: Vec<SeedRef> = ready
                .selected_ids
                .iter()
                .filter_map(|id| {
                    bundles
                        .iter()
                        .find(|b| &b.candidate_id == id)
                        .map(|b| SeedRef {
                            entry_id: b.entry_id.clone(),
                            entity: b.entity.clone(),
                        })
                })
                .collect();
            let metrics = score_ready(expect, &selected, &raw.decision);
            SeedSetCaseScore {
                case_id: case_id.into(),
                intent: intent.into(),
                metrics,
                selected_seeds: selected,
                reasoning,
            }
        }
        Ok(ValidatedSeedSelection::Abstain(abstain)) => {
            let decision = abstain.decision;
            SeedSetCaseScore {
                case_id: case_id.into(),
                intent: intent.into(),
                metrics: score_abstain(expect, decision),
                selected_seeds: vec![],
                reasoning,
            }
        }
        Err(_e) => SeedSetCaseScore {
            case_id: case_id.into(),
            intent: intent.into(),
            metrics: SeedSetMetrics {
                routing_error: true,
                ..Default::default()
            },
            selected_seeds: vec![],
            reasoning,
        },
    }
}

/// Per-case selector/resolve failure (BAML parse, out-of-range index, …).
#[cfg(feature = "llm-rerank")]
pub fn score_seed_selector_failure(case_id: &str, intent: &str, detail: &str) -> SeedSetCaseScore {
    SeedSetCaseScore {
        case_id: case_id.into(),
        intent: intent.into(),
        metrics: SeedSetMetrics {
            routing_error: true,
            ..Default::default()
        },
        selected_seeds: vec![],
        reasoning: Some(detail.into()),
    }
}

fn decision_label(d: SeedSelectionDecision) -> &'static str {
    match d {
        SeedSelectionDecision::Ready => "ready",
        SeedSelectionDecision::Clarify => "clarify",
        SeedSelectionDecision::HardMiss => "hard_miss",
    }
}

fn score_ready(
    expect: &SeedExpect,
    selected: &[SeedRef],
    decision: &SeedSelectionDecision,
) -> SeedSetMetrics {
    let decision_ok = expect.decision_any.iter().any(|d| d == "ready")
        && *decision == SeedSelectionDecision::Ready;
    let selected_set: HashSet<SeedRef> = selected.iter().cloned().collect();
    let bundle_exact = expect.acceptable_sets.iter().any(|set| {
        let set_refs: HashSet<SeedRef> = set.iter().cloned().collect();
        set_refs == selected_set
    });
    let false_open = !expect.acceptable_sets.is_empty()
        && *decision == SeedSelectionDecision::Ready
        && !bundle_exact;
    let excluded_violation = selected.iter().any(|s| expect.must_exclude.contains(s));
    let (precision, recall) = seed_pr_re(expect, selected);
    let max_ok = expect.max_seeds.unwrap_or(3);
    SeedSetMetrics {
        decision: decision_label(*decision).into(),
        decision_ok,
        bundle_exact,
        seed_precision: precision,
        seed_recall: recall,
        false_open,
        excluded_violation,
        seed_count: selected.len(),
        ..Default::default()
    }
    .with_seed_cap(max_ok, selected.len())
}

impl SeedSetMetrics {
    fn with_seed_cap(mut self, max: usize, count: usize) -> Self {
        if count > max {
            self.false_open = true;
        }
        self
    }
}

fn score_abstain(expect: &SeedExpect, decision: SeedSelectionDecision) -> SeedSetMetrics {
    let label = decision_label(decision);
    let decision_ok = expect.decision_any.iter().any(|d| d == label);
    SeedSetMetrics {
        decision: label.into(),
        decision_ok,
        false_open: false,
        ..Default::default()
    }
}

fn seed_pr_re(expect: &SeedExpect, selected: &[SeedRef]) -> (f64, f64) {
    if expect.acceptable_sets.is_empty() {
        return (1.0, 1.0);
    }
    let best = expect
        .acceptable_sets
        .iter()
        .map(|gold| {
            let gold_set: HashSet<_> = gold.iter().collect();
            let sel_set: HashSet<_> = selected.iter().collect();
            let tp = gold_set.intersection(&sel_set).count();
            let precision = if sel_set.is_empty() {
                0.0
            } else {
                tp as f64 / sel_set.len() as f64
            };
            let recall = if gold_set.is_empty() {
                1.0
            } else {
                tp as f64 / gold_set.len() as f64
            };
            (precision, recall)
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or((0.0, 0.0));
    best
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedSetAggregateReport {
    pub case_count: usize,
    pub bundle_exact_rate: f64,
    pub false_open_rate: f64,
    pub decision_accuracy: f64,
    pub routing_error_count: usize,
    pub mean_seeds_ready: f64,
    pub cases: Vec<SeedSetCaseScore>,
}

pub fn build_seed_aggregate(scores: &[SeedSetCaseScore]) -> SeedSetAggregateReport {
    let n = scores.len().max(1) as f64;
    let ready: Vec<_> = scores
        .iter()
        .filter(|s| s.metrics.decision == "ready")
        .collect();
    SeedSetAggregateReport {
        case_count: scores.len(),
        bundle_exact_rate: scores.iter().filter(|s| s.metrics.bundle_exact).count() as f64 / n,
        false_open_rate: scores.iter().filter(|s| s.metrics.false_open).count() as f64 / n,
        decision_accuracy: scores.iter().filter(|s| s.metrics.decision_ok).count() as f64 / n,
        routing_error_count: scores.iter().filter(|s| s.metrics.routing_error).count(),
        mean_seeds_ready: if ready.is_empty() {
            0.0
        } else {
            ready
                .iter()
                .map(|s| s.metrics.seed_count as f64)
                .sum::<f64>()
                / ready.len() as f64
        },
        cases: scores.to_vec(),
    }
}

pub fn format_seed_human_report(report: &SeedSetAggregateReport) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(
        &mut out,
        "Seed-set eval: {} cases | bundle_exact {:.1}% | false_open {:.1}% | decision_acc {:.1}% | routing_errors {}",
        report.case_count,
        report.bundle_exact_rate * 100.0,
        report.false_open_rate * 100.0,
        report.decision_accuracy * 100.0,
        report.routing_error_count,
    )
    .unwrap();
    for c in &report.cases {
        writeln!(
            &mut out,
            "[{}] decision={} exact={} false_open={} seeds={}",
            c.case_id,
            c.metrics.decision,
            c.metrics.bundle_exact,
            c.metrics.false_open,
            c.metrics.seed_count,
        )
        .unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_error_on_invalid_ids() {
        let expect = SeedExpect {
            decision_any: vec!["ready".into()],
            acceptable_sets: vec![vec![SeedRef {
                entry_id: "gmail".into(),
                entity: "Thread".into(),
            }]],
            ..Default::default()
        };
        let raw = SeedSelectionRaw {
            decision: SeedSelectionDecision::Ready,
            requirements: vec![],
            selected_ids: vec!["nope".into()],
            supporting_capability_ids: vec!["x".into()],
            alternative_sets: vec![],
            uncovered_requirements: vec![],
            reasoning: String::new(),
        };
        let score = score_seed_selection("t", "intent", &expect, &[], raw);
        assert!(score.metrics.routing_error);
    }
}
