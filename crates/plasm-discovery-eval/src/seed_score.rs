//! Seed-set eval scoring (`seed_expect` in cases.yaml).

use std::collections::HashSet;

use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_seed_select::{
    validate_seed_selection, validation_error_label, SeedSelectionDecision, SeedSelectionRaw,
    SeedSelectionValidationError, ValidatedSeedSelection,
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
    pub unsafe_open: bool,
    pub safe_ready: bool,
    pub excluded_violation: bool,
    pub routing_error: bool,
    pub symbol_hallucination: bool,
    pub raw_id_hallucination: bool,
    pub seed_count: usize,
    pub supporting_count: usize,
    pub clarify_to_ready: bool,
    pub hard_miss_to_ready: bool,
    pub gold_in_pool: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DecisionBranchMetrics {
    pub case_count: usize,
    pub decision_accuracy: f64,
    pub bundle_exact_rate: f64,
    pub unsafe_open_rate: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedSetCaseScore {
    pub case_id: String,
    pub intent: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_branch: Option<String>,
    pub metrics: SeedSetMetrics,
    pub selected_seeds: Vec<SeedRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<crate::seed_trace::SeedStageTrace>,
}

pub fn score_seed_selection(
    case_id: &str,
    intent: &str,
    tags: &[String],
    expect: &SeedExpect,
    bundles: &[EntityCandidateBundle],
    raw: SeedSelectionRaw,
    trace: Option<crate::seed_trace::SeedStageTrace>,
) -> SeedSetCaseScore {
    let reasoning = if raw.reasoning.is_empty() {
        None
    } else {
        Some(raw.reasoning.clone())
    };
    let expected_branch = expected_branch_label(&expect.decision_any);
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
            let metrics = score_ready(expect, &selected, &raw.decision, bundles);
            SeedSetCaseScore {
                case_id: case_id.into(),
                intent: intent.into(),
                tags: tags.to_vec(),
                expected_branch,
                metrics,
                selected_seeds: selected,
                reasoning,
                trace,
            }
        }
        Ok(ValidatedSeedSelection::Abstain(abstain)) => {
            let decision = abstain.decision;
            let mut metrics = score_abstain(expect, decision);
            // Abstain must still report pool membership — do not leave Default false when gold
            // survived retrieve (otherwise FO dashboards miscount Select abstains as pool misses).
            metrics.gold_in_pool =
                crate::seed_trace::SeedStageTrace::gold_in_pool(bundles, &expect.acceptable_sets);
            SeedSetCaseScore {
                case_id: case_id.into(),
                intent: intent.into(),
                tags: tags.to_vec(),
                expected_branch: expected_branch.clone(),
                metrics,
                selected_seeds: vec![],
                reasoning,
                trace,
            }
        }
        Err(e) => SeedSetCaseScore {
            case_id: case_id.into(),
            intent: intent.into(),
            tags: tags.to_vec(),
            expected_branch,
            metrics: score_validation_failure(&e),
            selected_seeds: vec![],
            reasoning,
            trace,
        },
    }
}

/// Per-case selector/resolve failure (BAML parse, out-of-range index, …).
pub fn score_seed_selector_failure(
    case_id: &str,
    intent: &str,
    tags: &[String],
    detail: &str,
    trace: Option<crate::seed_trace::SeedStageTrace>,
) -> SeedSetCaseScore {
    SeedSetCaseScore {
        case_id: case_id.into(),
        intent: intent.into(),
        tags: tags.to_vec(),
        expected_branch: None,
        metrics: SeedSetMetrics {
            routing_error: true,
            ..Default::default()
        },
        selected_seeds: vec![],
        reasoning: Some(detail.into()),
        trace,
    }
}

fn score_validation_failure(error: &SeedSelectionValidationError) -> SeedSetMetrics {
    let label = validation_error_label(error);
    SeedSetMetrics {
        routing_error: true,
        symbol_hallucination: label == "symbol_hallucination",
        raw_id_hallucination: label == "raw_id_hallucination",
        ..Default::default()
    }
}

fn decision_label(d: SeedSelectionDecision) -> &'static str {
    match d {
        SeedSelectionDecision::Ready => "ready",
        SeedSelectionDecision::Clarify => "clarify",
        SeedSelectionDecision::HardMiss => "hard_miss",
    }
}

fn expected_branch_label(decision_any: &[String]) -> Option<String> {
    if decision_any.is_empty() {
        return None;
    }
    let mut branches: Vec<&str> = decision_any
        .iter()
        .map(|d| d.as_str())
        .filter(|d| matches!(*d, "ready" | "clarify" | "hard_miss"))
        .collect();
    branches.sort_unstable();
    branches.dedup();
    if branches.len() == 1 {
        Some(branches[0].to_string())
    } else {
        Some("mixed".to_string())
    }
}

fn branch_metrics(scores: &[&SeedSetCaseScore], branch: &str) -> DecisionBranchMetrics {
    let subset: Vec<_> = scores
        .iter()
        .filter(|s| s.expected_branch.as_deref() == Some(branch))
        .copied()
        .collect();
    if subset.is_empty() {
        return DecisionBranchMetrics::default();
    }
    let n = subset.len() as f64;
    DecisionBranchMetrics {
        case_count: subset.len(),
        decision_accuracy: subset.iter().filter(|s| s.metrics.decision_ok).count() as f64 / n,
        bundle_exact_rate: subset.iter().filter(|s| s.metrics.bundle_exact).count() as f64 / n,
        unsafe_open_rate: subset.iter().filter(|s| s.metrics.unsafe_open).count() as f64 / n,
    }
}

fn tag_metrics(scores: &[&SeedSetCaseScore], tag: &str) -> DecisionBranchMetrics {
    let subset: Vec<_> = scores
        .iter()
        .filter(|s| s.tags.iter().any(|t| t == tag))
        .copied()
        .collect();
    if subset.is_empty() {
        return DecisionBranchMetrics::default();
    }
    let n = subset.len() as f64;
    DecisionBranchMetrics {
        case_count: subset.len(),
        decision_accuracy: subset.iter().filter(|s| s.metrics.decision_ok).count() as f64 / n,
        bundle_exact_rate: subset.iter().filter(|s| s.metrics.bundle_exact).count() as f64 / n,
        unsafe_open_rate: subset.iter().filter(|s| s.metrics.unsafe_open).count() as f64 / n,
    }
}

fn failure_stage_counts(scores: &[SeedSetCaseScore]) -> std::collections::BTreeMap<String, usize> {
    use crate::seed_trace::SeedFailureStage;
    let mut out = std::collections::BTreeMap::new();
    for score in scores {
        let Some(trace) = score.trace.as_ref() else {
            continue;
        };
        let Some(stage) = trace.failure_stage.as_ref() else {
            continue;
        };
        let key = match stage {
            SeedFailureStage::Retrieve => "retrieve",
            SeedFailureStage::Select => "select",
            SeedFailureStage::Resolve => "resolve",
            SeedFailureStage::Validate => "validate",
            SeedFailureStage::Score => "score",
        };
        *out.entry(key.to_string()).or_default() += 1;
    }
    out
}

fn score_ready(
    expect: &SeedExpect,
    selected: &[SeedRef],
    decision: &SeedSelectionDecision,
    bundles: &[EntityCandidateBundle],
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
    let gold_in_pool =
        crate::seed_trace::SeedStageTrace::gold_in_pool(bundles, &expect.acceptable_sets);
    let mut metrics = SeedSetMetrics {
        decision: decision_label(*decision).into(),
        decision_ok,
        bundle_exact,
        seed_precision: precision,
        seed_recall: recall,
        false_open,
        excluded_violation,
        seed_count: selected.len(),
        safe_ready: bundle_exact && !excluded_violation,
        gold_in_pool,
        ..Default::default()
    }
    .with_seed_cap(max_ok, selected.len());
    metrics.unsafe_open = metrics.unsafe_open_for(expect, decision);
    metrics
}

impl SeedSetMetrics {
    fn with_seed_cap(mut self, max: usize, count: usize) -> Self {
        if count > max {
            self.false_open = true;
        }
        self
    }

    fn unsafe_open_for(&self, expect: &SeedExpect, decision: &SeedSelectionDecision) -> bool {
        if *decision != SeedSelectionDecision::Ready {
            return false;
        }
        let expected_ready = expect.decision_any.iter().any(|d| d == "ready");
        let expected_clarify = expect.decision_any.iter().any(|d| d == "clarify");
        let expected_hard_miss = expect.decision_any.iter().any(|d| d == "hard_miss");
        // Ready is only "unsafe open" when the case forbids ready (clarify/hard_miss only).
        if !expected_ready && (expected_clarify || expected_hard_miss) {
            return true;
        }
        if expected_ready {
            return self.false_open || self.excluded_violation;
        }
        false
    }
}

fn score_abstain(expect: &SeedExpect, decision: SeedSelectionDecision) -> SeedSetMetrics {
    let label = decision_label(decision);
    let decision_ok = expect.decision_any.iter().any(|d| d == label);
    let clarify_to_ready = expect.decision_any.iter().any(|d| d == "clarify")
        && decision == SeedSelectionDecision::Ready;
    let hard_miss_to_ready = expect.decision_any.iter().any(|d| d == "hard_miss")
        && decision == SeedSelectionDecision::Ready;
    SeedSetMetrics {
        decision: label.into(),
        decision_ok,
        false_open: false,
        unsafe_open: clarify_to_ready || hard_miss_to_ready,
        clarify_to_ready,
        hard_miss_to_ready,
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
pub struct SeedRunVariance {
    pub runs: u32,
    pub bundle_exact_rate_mean: f64,
    pub bundle_exact_rate_std: f64,
    pub unsafe_open_rate_mean: f64,
    pub unsafe_open_rate_max: f64,
    pub decision_accuracy_mean: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedSetAggregateReport {
    pub case_count: usize,
    pub bundle_exact_rate: f64,
    pub false_open_rate: f64,
    /// Fraction of cases that opened ready (coverage of the selective predictor).
    pub ready_coverage: f64,
    /// False-open rate among ready cases only (risk at the observed coverage).
    pub false_open_at_coverage: f64,
    /// Fraction of cases that abstained with clarify.
    pub clarify_rate: f64,
    pub unsafe_open_rate: f64,
    pub safe_ready_rate: f64,
    pub decision_accuracy: f64,
    pub routing_error_count: usize,
    pub symbol_hallucination_count: usize,
    pub raw_id_hallucination_count: usize,
    pub hard_miss_to_ready_count: usize,
    pub clarify_to_ready_count: usize,
    pub excluded_violation_count: usize,
    pub gold_in_pool_rate: f64,
    pub mean_seeds_ready: f64,
    pub expected_ready: DecisionBranchMetrics,
    pub expected_clarify: DecisionBranchMetrics,
    pub expected_hard_miss: DecisionBranchMetrics,
    pub holdout: DecisionBranchMetrics,
    pub stratified_by_tag: std::collections::BTreeMap<String, DecisionBranchMetrics>,
    pub failure_stage_counts: std::collections::BTreeMap<String, usize>,
    pub gates_pass: bool,
    pub holdout_gates_pass: bool,
    pub coverage_ambiguous_acc: f64,
    pub coverage_gold_recall: f64,
    pub coverage_plan_recall: f64,
    pub coverage_entity_recall: f64,
    pub plan_select_exact_rate: f64,
    pub coverage_gates_pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_variance: Option<SeedRunVariance>,
    pub cases: Vec<SeedSetCaseScore>,
}

pub fn build_seed_aggregate(scores: &[SeedSetCaseScore]) -> SeedSetAggregateReport {
    build_seed_aggregate_inner(scores, None)
}

pub fn build_seed_aggregate_from_runs(
    per_run: &[SeedSetAggregateReport],
) -> SeedSetAggregateReport {
    let Some(first) = per_run.first() else {
        return build_seed_aggregate(&[]);
    };
    if per_run.len() == 1 {
        return first.clone();
    }
    let mut report = first.clone();
    let n = per_run.len() as f64;
    let mean =
        |f: fn(&SeedSetAggregateReport) -> f64| -> f64 { per_run.iter().map(f).sum::<f64>() / n };
    let std = |f: fn(&SeedSetAggregateReport) -> f64, m: f64| -> f64 {
        let var = per_run
            .iter()
            .map(|r| {
                let d = f(r) - m;
                d * d
            })
            .sum::<f64>()
            / n;
        var.sqrt()
    };
    let bundle_mean = mean(|r| r.bundle_exact_rate);
    let unsafe_mean = mean(|r| r.unsafe_open_rate);
    report.run_variance = Some(SeedRunVariance {
        runs: per_run.len() as u32,
        bundle_exact_rate_mean: bundle_mean,
        bundle_exact_rate_std: std(|r| r.bundle_exact_rate, bundle_mean),
        unsafe_open_rate_mean: unsafe_mean,
        unsafe_open_rate_max: per_run
            .iter()
            .map(|r| r.unsafe_open_rate)
            .fold(0.0_f64, f64::max),
        decision_accuracy_mean: mean(|r| r.decision_accuracy),
    });
    report.gates_pass = per_run.iter().all(|r| r.gates_pass);
    report.holdout_gates_pass = per_run.iter().all(|r| r.holdout_gates_pass);
    report
}

fn build_seed_aggregate_inner(
    scores: &[SeedSetCaseScore],
    run_variance: Option<SeedRunVariance>,
) -> SeedSetAggregateReport {
    let n = scores.len().max(1) as f64;
    let ready: Vec<_> = scores
        .iter()
        .filter(|s| s.metrics.decision == "ready")
        .collect();
    let false_open_rate = scores.iter().filter(|s| s.metrics.false_open).count() as f64 / n;
    let unsafe_open_rate = scores.iter().filter(|s| s.metrics.unsafe_open).count() as f64 / n;
    let hard_miss_to_ready_count = scores
        .iter()
        .filter(|s| s.metrics.hard_miss_to_ready)
        .count();
    let coverage_cases: Vec<_> = scores
        .iter()
        .filter_map(|s| s.trace.as_ref()?.coverage.as_ref())
        .collect();
    let coverage_n = coverage_cases.len().max(1) as f64;
    let coverage_ambiguous_acc = {
        let clarify_expected: Vec<_> = scores
            .iter()
            .filter(|s| s.expected_branch.as_deref() == Some("clarify"))
            .collect();
        if clarify_expected.is_empty() {
            1.0
        } else {
            clarify_expected
                .iter()
                .filter(|s| {
                    s.trace
                        .as_ref()
                        .and_then(|t| t.coverage.as_ref())
                        .is_some_and(|c| c.coverage_ambiguous)
                })
                .count() as f64
                / clarify_expected.len() as f64
        }
    };
    let coverage_gold_recall = coverage_cases
        .iter()
        .filter(|c| c.coverage_gold_recall)
        .count() as f64
        / coverage_n;
    let coverage_plan_recall = coverage_cases
        .iter()
        .filter(|c| c.coverage_plan_recall)
        .count() as f64
        / coverage_n;
    let coverage_entity_recall = coverage_cases
        .iter()
        .filter(|c| c.coverage_entity_recall)
        .count() as f64
        / coverage_n;
    let plan_select_exact_rate = coverage_cases
        .iter()
        .filter(|c| c.plan_select_exact)
        .count() as f64
        / coverage_n;
    let coverage_gates_pass = coverage_ambiguous_acc >= 0.8
        && coverage_plan_recall >= 0.95
        && plan_select_exact_rate >= 0.5;
    let gates_pass = false_open_rate <= 0.02
        && hard_miss_to_ready_count == 0
        && scores.iter().all(|s| !s.metrics.symbol_hallucination)
        && scores.iter().all(|s| !s.metrics.raw_id_hallucination);
    let score_refs: Vec<&SeedSetCaseScore> = scores.iter().collect();
    let expected_ready = branch_metrics(&score_refs, "ready");
    let expected_clarify = branch_metrics(&score_refs, "clarify");
    let expected_hard_miss = branch_metrics(&score_refs, "hard_miss");
    let holdout = tag_metrics(&score_refs, "holdout");
    let holdout_gates_pass = holdout.case_count == 0
        || (holdout.unsafe_open_rate == 0.0
            && scores
                .iter()
                .filter(|s| s.tags.iter().any(|t| t == "holdout"))
                .all(|s| !s.metrics.hard_miss_to_ready && !s.metrics.clarify_to_ready));
    let mut stratified_by_tag = std::collections::BTreeMap::new();
    for tag in scores
        .iter()
        .flat_map(|s| s.tags.iter())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
    {
        stratified_by_tag.insert(tag.clone(), tag_metrics(&score_refs, &tag));
    }
    let ready_expected: Vec<_> = scores
        .iter()
        .filter(|s| s.expected_branch.as_deref() == Some("ready"))
        .collect();
    let bundle_exact_rate = if ready_expected.is_empty() {
        0.0
    } else {
        ready_expected
            .iter()
            .filter(|s| s.metrics.bundle_exact)
            .count() as f64
            / ready_expected.len() as f64
    };
    let ready_coverage = ready.len() as f64 / n;
    let false_open_at_coverage = if ready.is_empty() {
        0.0
    } else {
        ready.iter().filter(|s| s.metrics.false_open).count() as f64 / ready.len() as f64
    };
    let clarify_rate = scores
        .iter()
        .filter(|s| s.metrics.decision == "clarify")
        .count() as f64
        / n;
    SeedSetAggregateReport {
        case_count: scores.len(),
        bundle_exact_rate,
        false_open_rate,
        ready_coverage,
        false_open_at_coverage,
        clarify_rate,
        unsafe_open_rate,
        safe_ready_rate: scores.iter().filter(|s| s.metrics.safe_ready).count() as f64 / n,
        decision_accuracy: scores.iter().filter(|s| s.metrics.decision_ok).count() as f64 / n,
        routing_error_count: scores.iter().filter(|s| s.metrics.routing_error).count(),
        symbol_hallucination_count: scores
            .iter()
            .filter(|s| s.metrics.symbol_hallucination)
            .count(),
        raw_id_hallucination_count: scores
            .iter()
            .filter(|s| s.metrics.raw_id_hallucination)
            .count(),
        hard_miss_to_ready_count,
        clarify_to_ready_count: scores.iter().filter(|s| s.metrics.clarify_to_ready).count(),
        excluded_violation_count: scores
            .iter()
            .filter(|s| s.metrics.excluded_violation)
            .count(),
        gold_in_pool_rate: scores.iter().filter(|s| s.metrics.gold_in_pool).count() as f64 / n,
        mean_seeds_ready: if ready.is_empty() {
            0.0
        } else {
            ready
                .iter()
                .map(|s| s.metrics.seed_count as f64)
                .sum::<f64>()
                / ready.len() as f64
        },
        expected_ready,
        expected_clarify,
        expected_hard_miss,
        holdout,
        stratified_by_tag,
        failure_stage_counts: failure_stage_counts(scores),
        gates_pass,
        holdout_gates_pass,
        coverage_ambiguous_acc,
        coverage_gold_recall,
        coverage_plan_recall,
        coverage_entity_recall,
        plan_select_exact_rate,
        coverage_gates_pass,
        run_variance,
        cases: scores.to_vec(),
    }
}

pub fn format_seed_human_report(report: &SeedSetAggregateReport) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(
        &mut out,
        "Seed-set eval: {} cases | ready_cov {:.1}% | FO@cov {:.1}% | clarify {:.1}% | bundle_exact {:.1}% | false_open {:.1}% | unsafe_open {:.1}% | safe_ready {:.1}% | decision_acc {:.1}% | hard_miss→ready {} | routing_errors {} | symbol_halluc {} | raw_id_halluc {} | gates {} | holdout_gates {} | coverage_ambiguous {:.1}% | coverage_plan_recall {:.1}% | coverage_entity_recall {:.1}% | plan_select_exact {:.1}% | coverage_gates {}",
        report.case_count,
        report.ready_coverage * 100.0,
        report.false_open_at_coverage * 100.0,
        report.clarify_rate * 100.0,
        report.bundle_exact_rate * 100.0,
        report.false_open_rate * 100.0,
        report.unsafe_open_rate * 100.0,
        report.safe_ready_rate * 100.0,
        report.decision_accuracy * 100.0,
        report.hard_miss_to_ready_count,
        report.routing_error_count,
        report.symbol_hallucination_count,
        report.raw_id_hallucination_count,
        if report.gates_pass { "PASS" } else { "FAIL" },
        if report.holdout_gates_pass { "PASS" } else { "FAIL" },
        report.coverage_ambiguous_acc * 100.0,
        report.coverage_plan_recall * 100.0,
        report.coverage_entity_recall * 100.0,
        report.plan_select_exact_rate * 100.0,
        if report.coverage_gates_pass { "PASS" } else { "FAIL" },
    )
    .unwrap();
    if let Some(variance) = &report.run_variance {
        writeln!(
            &mut out,
            "runs={} bundle_exact mean={:.1}%±{:.1}% unsafe_open mean={:.1}% max={:.1}% decision_acc mean={:.1}%",
            variance.runs,
            variance.bundle_exact_rate_mean * 100.0,
            variance.bundle_exact_rate_std * 100.0,
            variance.unsafe_open_rate_mean * 100.0,
            variance.unsafe_open_rate_max * 100.0,
            variance.decision_accuracy_mean * 100.0,
        )
        .unwrap();
    }
    writeln!(
        &mut out,
        "branch ready: n={} acc={:.1}% exact={:.1}% unsafe={:.1}% | clarify: n={} acc={:.1}% | hard_miss: n={} acc={:.1}%",
        report.expected_ready.case_count,
        report.expected_ready.decision_accuracy * 100.0,
        report.expected_ready.bundle_exact_rate * 100.0,
        report.expected_ready.unsafe_open_rate * 100.0,
        report.expected_clarify.case_count,
        report.expected_clarify.decision_accuracy * 100.0,
        report.expected_hard_miss.case_count,
        report.expected_hard_miss.decision_accuracy * 100.0,
    )
    .unwrap();
    if !report.failure_stage_counts.is_empty() {
        writeln!(
            &mut out,
            "failure_stages: {:?}",
            report.failure_stage_counts
        )
        .unwrap();
    }
    for c in &report.cases {
        writeln!(
            &mut out,
            "[{}] decision={} exact={} false_open={} unsafe={} seeds={} gold_pool={}",
            c.case_id,
            c.metrics.decision,
            c.metrics.bundle_exact,
            c.metrics.false_open,
            c.metrics.unsafe_open,
            c.metrics.seed_count,
            c.metrics.gold_in_pool,
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
        let score = score_seed_selection("t", "intent", &[], &expect, &[], raw, None);
        assert!(score.metrics.routing_error);
    }
}
