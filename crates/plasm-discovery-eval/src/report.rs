use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context;
use serde::Serialize;

use crate::score::{CaseMetrics, CaseScore};

#[derive(Debug, Clone, Serialize)]
pub struct AggregateReport {
    pub case_count: usize,
    pub hit_at_1_entry_rate: f64,
    pub hit_at_3_entry_rate: f64,
    pub mean_mrr_entry: f64,
    pub mean_noise_at_10: f64,
    pub hard_miss_count: usize,
    pub soft_noise_count: usize,
    pub ok_count: usize,
    pub cases: Vec<CaseReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
    pub id: String,
    pub intent: String,
    pub metrics: CaseMetrics,
    pub baseline_top: Vec<TopRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranked_top: Option<Vec<TopRow>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopRow {
    pub entry_id: String,
    pub entity: String,
    pub capability_name: String,
    pub score: u32,
}

pub fn build_aggregate(scores: &[CaseScore]) -> AggregateReport {
    let n = scores.len().max(1) as f64;
    AggregateReport {
        case_count: scores.len(),
        hit_at_1_entry_rate: scores.iter().filter(|s| s.metrics.hit_at_1_entry).count() as f64 / n,
        hit_at_3_entry_rate: scores.iter().filter(|s| s.metrics.hit_at_3_entry).count() as f64 / n,
        mean_mrr_entry: scores.iter().map(|s| s.metrics.mrr_entry).sum::<f64>() / n,
        mean_noise_at_10: scores
            .iter()
            .map(|s| s.metrics.noise_at_k as f64)
            .sum::<f64>()
            / n,
        hard_miss_count: scores
            .iter()
            .filter(|s| s.metrics.failure_class == "hard_miss")
            .count(),
        soft_noise_count: scores
            .iter()
            .filter(|s| s.metrics.failure_class == "soft_noise")
            .count(),
        ok_count: scores
            .iter()
            .filter(|s| s.metrics.failure_class == "ok")
            .count(),
        cases: scores
            .iter()
            .map(|s| CaseReport {
                id: s.case_id.clone(),
                intent: s.intent.clone(),
                metrics: s.metrics.clone(),
                baseline_top: s.baseline_top.iter().map(row_from).collect(),
                reranked_top: s
                    .reranked_top
                    .as_ref()
                    .map(|v| v.iter().map(row_from).collect()),
            })
            .collect(),
    }
}

fn row_from(c: &plasm_core::RankedCandidate) -> TopRow {
    TopRow {
        entry_id: c.entry_id.clone(),
        entity: c.entity.clone(),
        capability_name: c.capability_name.clone(),
        score: c.score,
    }
}

pub fn write_json_report(path: &Path, report: &AggregateReport) -> anyhow::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(report)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn format_human_report(report: &AggregateReport) -> String {
    let mut out = String::new();
    writeln!(
        &mut out,
        "Discovery eval: {} cases | hit@1_entry {:.1}% | hit@3_entry {:.1}% | MRR {:.3} | mean noise@10 {:.2}",
        report.case_count,
        report.hit_at_1_entry_rate * 100.0,
        report.hit_at_3_entry_rate * 100.0,
        report.mean_mrr_entry,
        report.mean_noise_at_10,
    ).unwrap();
    writeln!(
        &mut out,
        "Failure classes: ok={} hard_miss={} soft_noise={}",
        report.ok_count, report.hard_miss_count, report.soft_noise_count
    )
    .unwrap();
    for c in &report.cases {
        writeln!(
            &mut out,
            "\n[{}] {} — hit@1={} hit@3={} class={}",
            c.id,
            trunc(&c.intent, 64),
            c.metrics.hit_at_1_entry,
            c.metrics.hit_at_3_entry,
            c.metrics.failure_class
        )
        .unwrap();
        if let Some(top) = c
            .reranked_top
            .as_ref()
            .and_then(|v| v.first())
            .or_else(|| c.baseline_top.first())
        {
            writeln!(
                &mut out,
                "  top: {}:{}:{} score={}",
                top.entry_id, top.entity, top.capability_name, top.score
            )
            .unwrap();
        }
    }
    out
}

pub fn write_human_report(path: &Path, report: &AggregateReport) -> anyhow::Result<()> {
    std::fs::write(path, format_human_report(report))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
