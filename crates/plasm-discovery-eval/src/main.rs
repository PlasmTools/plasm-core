use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
#[cfg(feature = "llm-rerank")]
use plasm_discovery_eval::build_seed_aggregate_from_runs;
use plasm_discovery_eval::{
    build_aggregate, build_seed_aggregate, default_cases_path, default_catalogs_path,
    format_human_report, format_seed_human_report, load_cases, load_catalog_entry_ids,
    load_registry, resolve_apis_root, score_all_baseline, write_human_report, write_json_report,
};
use plasm_eval_common::model_slug;

#[derive(Parser, Debug)]
#[command(name = "plasm-discovery-eval")]
struct Args {
    #[arg(long)]
    cases: Option<PathBuf>,
    #[arg(long)]
    catalogs: Option<PathBuf>,
    #[arg(long)]
    apis_root: Option<PathBuf>,
    #[arg(long, default_value_t = 24)]
    rerank_k: usize,
    #[arg(long, default_value_t = false)]
    baseline_only: bool,
    #[arg(long, default_value_t = false)]
    report: bool,
    #[arg(long, default_value_t = false)]
    seed_set_eval: bool,
    #[arg(long, default_value = "google/gemma-3-4b-it")]
    model: String,
    #[arg(long, default_value_t = plasm_eval_common::DEFAULT_OPENROUTER_EVAL_TEMPERATURE)]
    temperature: f64,
    #[arg(long, default_value_t = plasm_eval_common::DEFAULT_OPENROUTER_EVAL_SEED)]
    seed: u64,
    #[arg(long, default_value_t = 1)]
    runs: u32,
    #[arg(long, default_value_t = false)]
    coverage_shadow_only: bool,
    /// Sweep READY_MARGIN ∈ {0..5} on the loaded cases (prefer holdout) and print risk-coverage.
    #[arg(long, default_value_t = false)]
    margin_sweep: bool,
    #[arg(long, default_value_t = false)]
    gate_check: bool,
}

fn coverage_shadow_report_stem(cases_path: &std::path::Path) -> String {
    let default = default_cases_path();
    if cases_path == default {
        return "coverage-shadow".into();
    }
    let stem = cases_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cases");
    if stem == "cases" {
        "coverage-shadow".into()
    } else if let Some(suffix) = stem.strip_prefix("cases-") {
        format!("coverage-shadow-{suffix}")
    } else {
        format!("coverage-shadow-{stem}")
    }
}

#[cfg(feature = "llm-rerank")]
fn seed_report_stem(cases_path: &std::path::Path, model: &str) -> String {
    let slug = plasm_eval_common::model_slug(model);
    let default = default_cases_path();
    if cases_path == default {
        return format!("{slug}-seed-set");
    }
    let stem = cases_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cases");
    if stem == "cases" {
        format!("{slug}-seed-set")
    } else if let Some(suffix) = stem.strip_prefix("cases-") {
        format!("{slug}-seed-set-{suffix}")
    } else {
        format!("{slug}-seed-set-{stem}")
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cases_path = args.cases.clone().unwrap_or_else(default_cases_path);
    let catalogs_path = args.catalogs.clone().unwrap_or_else(default_catalogs_path);
    let apis_root = resolve_apis_root(args.apis_root.as_deref());

    let cases = load_cases(&cases_path)?;
    let entry_ids = load_catalog_entry_ids(&catalogs_path)?;
    let registry = load_registry(&apis_root, &entry_ids)?;

    eprintln!("loaded {} cases, {} catalogs", cases.len(), entry_ids.len());

    if args.margin_sweep {
        let allowed: Vec<String> = registry.entry_ids();
        let points = plasm_discovery_eval::sweep_ready_margins(
            &registry,
            &cases,
            &allowed,
            &[0, 1, 2, 3, 4, 5],
        )?;
        println!("READY_MARGIN sweep (risk–coverage)");
        println!("margin\tready_cov\tFO@cov\tFO_all\tdecision_acc\thm→ready\tplan_select");
        for p in &points {
            println!(
                "{}\t{:.1}%\t{:.1}%\t{:.1}%\t{:.1}%\t{}\t{:.1}%",
                p.margin,
                p.ready_coverage * 100.0,
                p.false_open_at_coverage * 100.0,
                p.false_open_rate * 100.0,
                p.decision_accuracy * 100.0,
                p.hard_miss_to_ready_count,
                p.plan_select_exact_rate * 100.0,
            );
        }
        if args.report {
            let dir = cases_path.parent().context("cases parent")?;
            std::fs::write(
                dir.join("ready-margin-sweep.latest.json"),
                serde_json::to_string_pretty(&points)?,
            )?;
        }
        return Ok(());
    }

    if args.coverage_shadow_only {
        let allowed: Vec<String> = registry.entry_ids();
        let scores = plasm_discovery_eval::score_all_coverage_shadow(&registry, &cases, &allowed)?;
        let report = build_seed_aggregate(&scores);
        println!("{}", format_seed_human_report(&report));
        if args.report {
            let stem = coverage_shadow_report_stem(&cases_path);
            let dir = cases_path.parent().context("cases parent")?;
            std::fs::write(
                dir.join(format!("{stem}.latest.json")),
                serde_json::to_string_pretty(&report)?,
            )?;
            std::fs::write(
                dir.join(format!("{stem}.latest.human.txt")),
                format_seed_human_report(&report),
            )?;
        }
        return Ok(());
    }

    if args.seed_set_eval {
        #[cfg(feature = "llm-rerank")]
        {
            let api_key =
                std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY required")?;
            let mut per_run = Vec::new();
            for run in 0..args.runs {
                let run_seed = args.seed.wrapping_add(u64::from(run));
                let scores = plasm_discovery_eval::score_all_seed_sets(
                    &registry,
                    &cases,
                    &args.model,
                    &api_key,
                    args.temperature,
                    run_seed,
                )?;
                per_run.push(build_seed_aggregate(&scores));
            }
            let report = if args.runs == 1 {
                per_run.into_iter().next().unwrap()
            } else {
                build_seed_aggregate_from_runs(&per_run)
            };
            println!("{}", format_seed_human_report(&report));
            if args.gate_check && (!report.gates_pass || !report.holdout_gates_pass) {
                anyhow::bail!("seed-set safety gates failed");
            }
            if args.report {
                let stem = seed_report_stem(&cases_path, &args.model);
                let dir = cases_path.parent().context("cases parent")?;
                std::fs::write(
                    dir.join(format!("{stem}.latest.json")),
                    serde_json::to_string_pretty(&report)?,
                )?;
                std::fs::write(
                    dir.join(format!("{stem}.latest.human.txt")),
                    format_seed_human_report(&report),
                )?;
            }
            return Ok(());
        }
        #[cfg(not(feature = "llm-rerank"))]
        {
            anyhow::bail!("seed-set eval requires --features llm-rerank");
        }
    }

    let scores = if args.baseline_only {
        score_all_baseline(&registry, &cases, args.rerank_k)
    } else {
        #[cfg(feature = "llm-rerank")]
        {
            let api_key =
                std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY required")?;
            plasm_discovery_eval::score_all_with_rerank(
                &registry,
                &cases,
                args.rerank_k,
                &args.model,
                &api_key,
                args.temperature,
                args.seed,
            )?
        }
        #[cfg(not(feature = "llm-rerank"))]
        {
            anyhow::bail!("rerank requires --features llm-rerank");
        }
    };

    let report = build_aggregate(&scores);
    println!("{}", format_human_report(&report));

    if args.report {
        let stem = if args.baseline_only {
            "baseline".into()
        } else {
            model_slug(&args.model)
        };
        let dir = cases_path.parent().context("cases parent")?;
        write_json_report(&dir.join(format!("{stem}.latest.json")), &report)?;
        write_human_report(&dir.join(format!("{stem}.latest.human.txt")), &report)?;
    }
    Ok(())
}
