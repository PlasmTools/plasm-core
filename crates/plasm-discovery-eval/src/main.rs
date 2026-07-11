use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use plasm_discovery_eval::{
    build_aggregate, default_cases_path, default_catalogs_path,
    format_human_report, load_cases, load_catalog_entry_ids,
    load_registry, resolve_apis_root, score_all_baseline, write_human_report, write_json_report,
};
#[cfg(feature = "llm-rerank")]
use plasm_discovery_eval::{build_seed_aggregate, format_seed_human_report};
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

    if args.seed_set_eval {
        #[cfg(feature = "llm-rerank")]
        {
            let api_key =
                std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY required")?;
            let scores = plasm_discovery_eval::score_all_seed_sets(
                &registry,
                &cases,
                &args.model,
                &api_key,
                args.temperature,
                args.seed,
            )?;
            let report = build_seed_aggregate(&scores);
            println!("{}", format_seed_human_report(&report));
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
