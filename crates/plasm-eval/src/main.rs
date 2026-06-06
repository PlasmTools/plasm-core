//! CLI: load CGS + eval cases, call BAML `TranslatePlan`, validate with correction rounds, emit JSON scores.
//! Subcommands: `coverage` (CGS vs `covers`), `scaffold` (starter YAML). Default: run eval harness.

mod dotenv_safe;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use plasm_core::domain_lexicon::DomainLexicon;
use plasm_core::loader::load_schema_dir;
use plasm_core::CGS;
use plasm_core::{PromptPipelineConfig, PromptRenderMode};
use plasm_eval::baml_client::sync_client::B;
use plasm_eval::baml_client::types::{PlanChatTurn, Union2KassistantOrKuser};
use plasm_eval::baml_client::ClientRegistry;
use plasm_eval::{
    apply_coverage_override, build_coverage_report, cases_with_effective_covers,
    compare_case_covers_to_derived, load_cases_dir, load_cases_file, print_coverage_text,
    required_domain_entities, required_form_buckets, union_case_covers, union_case_entities,
    validate_case_covers_against_allowed, validate_case_entities_against_schema, CoverageOverride,
    CoversSource, EvalCase, EvalFormId,
};
use plasm_eval::{
    build_correction_feedback, build_correction_metrics, failed_semantic_case_score,
    finalize_case_score, finalize_score, nl_translate_user_bundle, openrouter_eval_llm_options,
    score_case, validate_plan_steps_with_lexicon_detailed, EvalAttemptReport, EvalPlanStep,
    DEFAULT_OPENROUTER_EVAL_SEED, DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
struct PromptStatsSnapshot {
    prompt_chars: usize,
    /// `o200k_base` (local riptoken), closer to API usage than `chars/4`.
    prompt_token_est: usize,
    /// Legacy `chars/4` rough line.
    prompt_token_est_div4: usize,
    json_tool_capability_count: usize,
    json_tool_navigation_count: usize,
    json_tool_estimate: usize,
}

impl From<plasm_core::prompt_render::PromptSurfaceStats> for PromptStatsSnapshot {
    fn from(v: plasm_core::prompt_render::PromptSurfaceStats) -> Self {
        Self {
            prompt_chars: v.prompt_chars,
            prompt_token_est: v.prompt_tokens_o200k,
            prompt_token_est_div4: v.token_estimate,
            json_tool_capability_count: v.capability_tools,
            json_tool_navigation_count: v.navigation_tools,
            json_tool_estimate: v.json_tool_estimate,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "plasm-eval",
    about = "LLM eval harness, coverage check, and eval YAML scaffold"
)]
struct Top {
    #[command(subcommand)]
    sub: Option<EvalSubcommand>,
    /// When no subcommand is given, runs the eval harness (requires --schema and --cases).
    #[command(flatten)]
    run: RunArgs,
}

#[derive(clap::Args, Debug, Clone)]
#[command(next_help_heading = "Eval run (default)")]
struct RunArgs {
    /// Path to schema directory (e.g. fixtures/schemas/petstore)
    #[arg(long)]
    schema: Option<PathBuf>,
    /// YAML file or directory of case files
    #[arg(long)]
    cases: Option<PathBuf>,
    /// Focus entity for prompt rendering (optional)
    #[arg(long)]
    focus: Option<String>,
    /// Prompt render mode for schema/session instructions.
    #[arg(long, default_value = "tsv", value_parser = ["compact", "tsv"])]
    symbol_tuning: String,
    /// Load schema + cases only; print prompt stats, no LLM
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    /// Print full teaching table prompt to stdout and exit (no `--cases`; no LLM). Same string as eval / REPL.
    #[arg(long, default_value_t = false)]
    print_prompt: bool,
    /// Print teaching prompt as TSV (expression-first table) and exit.
    #[arg(long, default_value_t = false)]
    print_prompt_tsv: bool,
    /// Total LLM calls per case: 1 = no correction; 2+ = retry with structured errors + hints.
    #[arg(long, default_value_t = 2)]
    attempts: u32,
    /// OpenRouter model ID (e.g. `google/gemma-3-4b-it`). Default matches `baml_src/clients.baml` Sonnet.
    #[arg(long, default_value = "anthropic/claude-3.5-sonnet")]
    model: String,
    /// Sampling temperature on the OpenRouter (`openai-generic`) request (default matches `baml_src/clients.baml`).
    #[arg(long, default_value_t = DEFAULT_OPENROUTER_EVAL_TEMPERATURE)]
    temperature: f64,
    /// Request seed on OpenRouter / upstream (best-effort; not all models honor it).
    #[arg(long, default_value_t = DEFAULT_OPENROUTER_EVAL_SEED)]
    seed: u64,
    /// Optional extra copy: also write under `<dir>/<schema>/` (timestamped stems + `{model}.latest.*`).
    /// Default: writes `{model-slug}.latest.human.txt` and `{model-slug}.latest.json` next to `--cases`.
    #[arg(long)]
    report_dir: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum EvalSubcommand {
    /// Compare CGS-derived required buckets to per-case `covers` (optionally derived from `reference_expr`).
    Coverage(CoverageArgs),
    /// Print a starter eval/cases YAML fragment (stdout) from CGS — fill goals and expect blocks.
    Scaffold(ScaffoldArgs),
}

#[derive(clap::Args, Debug)]
struct CoverageArgs {
    /// Path to schema directory (e.g. fixtures/schemas/petstore)
    #[arg(long)]
    schema: PathBuf,
    /// YAML file or directory of eval case files
    #[arg(long)]
    cases: PathBuf,
    /// `text` (markdown-ish, human/LLM friendly) or `json`
    #[arg(long, default_value = "text")]
    format: String,
    /// How to combine YAML `covers` with [`reference_expr`](EvalCase::reference_expr) for the coverage union.
    #[arg(long, value_enum, default_value_t = CoversSourceArg::Yaml)]
    covers_source: CoversSourceArg,
    /// Parse `reference_expr` and fail if derived [`EvalFormId`] set differs from YAML `covers` (see `--compare-derived-allow-extra-claims`).
    #[arg(long, default_value_t = false)]
    compare_derived: bool,
    /// With `--compare-derived`, require only that YAML `covers` is a superset of the derived set (extra claims allowed).
    #[arg(long, default_value_t = false)]
    compare_derived_allow_extra_claims: bool,
    /// Optional coverage override — defaults to `apis/<schema>/eval/coverage.yaml`, else `eval/coverage/<schema>.yaml`
    #[arg(long)]
    coverage_config: Option<PathBuf>,
    /// Exit 0 even when required buckets are missing (warnings only)
    #[arg(long, default_value_t = false)]
    warn_only: bool,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CoversSourceArg {
    #[default]
    Yaml,
    Reference,
    Merge,
}

impl From<CoversSourceArg> for CoversSource {
    fn from(v: CoversSourceArg) -> Self {
        match v {
            CoversSourceArg::Yaml => CoversSource::Yaml,
            CoversSourceArg::Reference => CoversSource::Reference,
            CoversSourceArg::Merge => CoversSource::Merge,
        }
    }
}

#[derive(clap::Args, Debug)]
struct ScaffoldArgs {
    #[arg(long)]
    schema: PathBuf,
    /// Write to `<schema>/eval/cases.yaml` (creates `eval/` if needed) instead of stdout.
    #[arg(long)]
    write: bool,
    /// With `--write`, overwrite an existing `cases.yaml`.
    #[arg(long)]
    force: bool,
}

fn run_one_case(
    cgs: &CGS,
    prompt: &str,
    case: &EvalCase,
    max_attempts: u32,
    registry: &ClientRegistry,
    pipeline: &PromptPipelineConfig,
    // One transcript for the whole run: first user = teaching table + goal; later users = `--- GOAL ---` only;
    // assistant = Plasm `text` only (no `reasoning`) to keep per-request size bounded.
    chat_session: &mut Vec<PlanChatTurn>,
) -> anyhow::Result<serde_json::Value> {
    let mut correction_context = String::new();
    let mut first_success_at: Option<u32> = None;
    let mut final_parsed = None;
    let mut last_failure_json: Option<serde_json::Value> = None;
    let mut rounds_used = 0u32;
    let mut attempt_trace: Vec<EvalAttemptReport> = Vec::new();
    let lexicon = DomainLexicon::from_cgs(cgs);

    for attempt in 0..max_attempts {
        rounds_used = attempt + 1;
        let correction_context_in = if attempt == 0 {
            None
        } else {
            Some(correction_context.clone())
        };
        let first_turn = chat_session.is_empty();
        let user_content = nl_translate_user_bundle(
            first_turn,
            prompt,
            case.goal.as_str(),
            correction_context.as_str(),
        );
        let mut translate_messages = chat_session.clone();
        translate_messages.push(PlanChatTurn {
            role: Union2KassistantOrKuser::Kuser,
            content: user_content,
        });
        let plan = B
            .TranslatePlan
            .with_client_registry(registry)
            .call(translate_messages.as_slice())
            .map_err(|e| anyhow::anyhow!("BAML TranslatePlan: {e}"))
            .with_context(|| format!("LLM call for case {} attempt {}", case.id, attempt))?;

        let text = plan.text.trim().to_string();
        let reasoning = plan.reasoning.trim().to_string();
        let steps = vec![EvalPlanStep {
            text: text.clone(),
            reasoning: reasoning.clone(),
        }];
        let texts = vec![text.clone()];
        let step_pairs: Vec<(&str, &str)> = vec![(text.as_str(), reasoning.as_str())];

        let (validation, lexicon_notes) =
            validate_plan_steps_with_lexicon_detailed(cgs, &texts, &lexicon, pipeline, None);
        // First user: teaching table + `--- GOAL ---`; later users: goal only. Assistant: backtick `text` only
        // (keeps later LLM calls from re-processing long reasoning; full steps stay in `attempt_trace`).
        // On validation failure we must still append (user, assistant) or correction rounds resend the teaching table.
        let user_hist = if first_turn {
            format!("{prompt}\n--- GOAL ---\n{}", case.goal)
        } else {
            format!("--- GOAL ---\n{}", case.goal)
        };
        let assistant_hist = format!("`{text}`");
        match validation {
            Ok(parsed) => {
                chat_session.push(PlanChatTurn {
                    role: Union2KassistantOrKuser::Kuser,
                    content: user_hist,
                });
                chat_session.push(PlanChatTurn {
                    role: Union2KassistantOrKuser::Kassistant,
                    content: assistant_hist,
                });
                attempt_trace.push(EvalAttemptReport {
                    attempt: attempt + 1,
                    steps,
                    validation_ok: true,
                    diagnostics: None,
                    correction_context_in,
                    lexicon_notes,
                });
                first_success_at = Some(attempt);
                final_parsed = Some(parsed);
                last_failure_json = None;
                break;
            }
            Err(diags) => {
                chat_session.push(PlanChatTurn {
                    role: Union2KassistantOrKuser::Kuser,
                    content: user_hist,
                });
                chat_session.push(PlanChatTurn {
                    role: Union2KassistantOrKuser::Kassistant,
                    content: assistant_hist,
                });
                last_failure_json = serde_json::to_value(&diags).ok();
                attempt_trace.push(EvalAttemptReport {
                    attempt: attempt + 1,
                    steps,
                    validation_ok: false,
                    diagnostics: Some(diags.clone()),
                    correction_context_in,
                    lexicon_notes,
                });
                correction_context =
                    build_correction_feedback(&case.goal, attempt as usize, &step_pairs, &diags);
                if attempt + 1 == max_attempts {
                    final_parsed = None;
                }
            }
        }
    }

    let final_ok = final_parsed.is_some();
    let parse_ok = final_ok;
    let tc_ok = final_ok;

    let winning_step_text = attempt_trace
        .iter()
        .rev()
        .find(|a| a.validation_ok)
        .map(|a| {
            a.steps
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });

    let mut sc = if let Some(ref parsed) = final_parsed {
        score_case(&case.expect, parsed, winning_step_text.as_deref())
    } else {
        failed_semantic_case_score()
    };

    if max_attempts > 1 {
        let cm =
            build_correction_metrics(rounds_used, first_success_at, final_ok, last_failure_json);
        sc = finalize_case_score(
            sc,
            case.id.clone(),
            case.tags.clone(),
            parse_ok,
            tc_ok,
            Some(cm),
            case.expect.correction.weight,
        );
    } else {
        sc = finalize_score(sc, case.id.clone(), case.tags.clone(), parse_ok, tc_ok);
    }

    sc.goal = case.goal.clone();
    sc.reference_expr = case.reference_expr.clone();
    sc.attempt_trace = attempt_trace;

    Ok(serde_json::to_value(&sc)?)
}

fn cmd_coverage(args: CoverageArgs) -> anyhow::Result<()> {
    let cgs = load_schema_dir(&args.schema).map_err(|e| anyhow::anyhow!("load schema: {e}"))?;
    plasm_compile::validate_cgs_capability_templates(&cgs)
        .map_err(|e| anyhow::anyhow!("invalid CML capability templates: {e}"))?;

    let schema_key = args
        .schema
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let mut case_list: Vec<EvalCase> = if args.cases.is_dir() {
        load_cases_dir(&args.cases)?
    } else {
        load_cases_file(&args.cases)?
    };
    case_list.retain(|c| c.schema == schema_key);

    let mut required = required_form_buckets(&cgs);
    let required_entities = required_domain_entities(&cgs);

    let canon_cov = PathBuf::from("apis")
        .join(&schema_key)
        .join("eval")
        .join("coverage.yaml");
    let legacy_cov = PathBuf::from("eval/coverage").join(format!("{schema_key}.yaml"));
    let cfg_path = args
        .coverage_config
        .clone()
        .filter(|p| p.exists())
        .or_else(|| canon_cov.exists().then_some(canon_cov))
        .or_else(|| legacy_cov.exists().then_some(legacy_cov));

    if let Some(ref p) = cfg_path {
        let text = std::fs::read_to_string(p)
            .with_context(|| format!("read coverage config {}", p.display()))?;
        let o: CoverageOverride = serde_yaml::from_str(&text)
            .with_context(|| format!("parse coverage config {}", p.display()))?;
        if !o.schema.is_empty() && o.schema != schema_key {
            anyhow::bail!(
                "coverage config schema {:?} != schema dir {:?}",
                o.schema,
                schema_key
            );
        }
        required = apply_coverage_override(required, &o)?;
    }

    let allowed: HashSet<EvalFormId> = required.keys().copied().collect();

    validate_case_entities_against_schema(&case_list, &schema_key, &cgs)?;

    if args.compare_derived {
        compare_case_covers_to_derived(
            &case_list,
            &schema_key,
            &cgs,
            args.compare_derived_allow_extra_claims,
        )?;
    }

    let effective_cases = cases_with_effective_covers(&case_list, &cgs, args.covers_source.into())?;
    validate_case_covers_against_allowed(&effective_cases, &schema_key, &allowed)?;

    let (union, by_case) = union_case_covers(&effective_cases, &schema_key);
    let (entity_union, by_case_entities) = union_case_entities(
        &effective_cases,
        &schema_key,
        &cgs,
        args.covers_source.into(),
    )?;
    let report = build_coverage_report(
        &schema_key,
        &required,
        &union,
        &by_case,
        &required_entities,
        &entity_union,
        &by_case_entities,
    );

    match args.format.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        _ => print_coverage_text(&report),
    }

    if !report.ok && !args.warn_only {
        let mut parts = Vec::new();
        if !report.missing.is_empty() {
            parts.push(format!(
                "{} missing form bucket(s): {}",
                report.missing.len(),
                report.missing.join(", ")
            ));
        }
        if !report.entities_missing.is_empty() {
            parts.push(format!(
                "{} missing entity coverage: {}",
                report.entities_missing.len(),
                report.entities_missing.join(", ")
            ));
        }
        anyhow::bail!("coverage incomplete: {}", parts.join("; "));
    }
    Ok(())
}

fn cmd_scaffold(args: ScaffoldArgs) -> anyhow::Result<()> {
    let cgs = load_schema_dir(&args.schema).map_err(|e| anyhow::anyhow!("load schema: {e}"))?;
    plasm_compile::validate_cgs_capability_templates(&cgs)
        .map_err(|e| anyhow::anyhow!("invalid CML capability templates: {e}"))?;

    let schema_key = args
        .schema
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("schema");

    let yaml = plasm_eval::scaffold_cases_yaml(&cgs, schema_key);

    if args.write {
        let out_path = args.schema.join("eval").join("cases.yaml");
        if out_path.exists() && !args.force {
            anyhow::bail!(
                "{} already exists; pass --force to overwrite",
                out_path.display()
            );
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &yaml)?;
        eprintln!("Wrote {}", out_path.display());
    } else {
        print!("{yaml}");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    dotenv_safe::load_from_cwd_parents();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "plasm_core::loader=info,plasm_core::prompt_render=info,plasm_compile::transport=info,info",
                )
            }),
        )
        .try_init();

    let top = Top::parse();

    match top.sub {
        Some(EvalSubcommand::Coverage(c)) => cmd_coverage(c),
        Some(EvalSubcommand::Scaffold(s)) => cmd_scaffold(s),
        None => {
            let run = top.run;
            if run.print_prompt || run.print_prompt_tsv {
                let schema = run
                    .schema
                    .clone()
                    .context("--print-prompt/--print-prompt-tsv requires --schema")?;
                let cgs =
                    load_schema_dir(&schema).map_err(|e| anyhow::anyhow!("load schema: {e}"))?;
                plasm_compile::validate_cgs_capability_templates(&cgs)
                    .map_err(|e| anyhow::anyhow!("invalid CML capability templates: {e}"))?;
                let render_mode = if run.print_prompt_tsv {
                    PromptRenderMode::Tsv
                } else {
                    PromptRenderMode::parse_user_facing_or_default(run.symbol_tuning.as_str())
                };
                let pipeline = PromptPipelineConfig::for_cli_focus(run.focus.as_deref())
                    .with_render_mode(render_mode);
                let prompt = if run.print_prompt_tsv {
                    pipeline.render_prompt_tsv(&cgs, None)
                } else {
                    pipeline.render_prompt(&cgs, None)
                };
                let st = pipeline.prompt_surface_stats(&cgs, None, &prompt);
                // Write prompt first so a line-buffered terminal shows teaching table immediately; stats on
                // stderr last so they stay visible below the bundle (and after tracing lines).
                print!("{prompt}");
                std::io::stdout().flush().context(
                    "flush stdout after --print-prompt/--print-prompt-tsv teaching table",
                )?;
                eprintln!("\nplasm-eval: schema prompt — {}", st.summary_line_body());
                std::io::stderr()
                    .flush()
                    .context("flush stderr after schema prompt summary")?;
                return Ok(());
            }
            let schema = run
                .schema
                .clone()
                .context("eval run: --schema and --cases are required (or use `plasm-eval coverage` / `scaffold`)")?;
            let cases = run.cases.clone().context("eval run: --cases is required")?;
            run_eval_harness(schema, cases, run)
        }
    }
}

fn run_eval_harness(schema: PathBuf, cases: PathBuf, cli: RunArgs) -> anyhow::Result<()> {
    let cgs = load_schema_dir(&schema).map_err(|e| anyhow::anyhow!("load schema: {e}"))?;
    plasm_compile::validate_cgs_capability_templates(&cgs)
        .map_err(|e| anyhow::anyhow!("invalid CML capability templates: {e}"))?;

    let mut case_list: Vec<EvalCase> = if cases.is_dir() {
        load_cases_dir(&cases)?
    } else {
        load_cases_file(&cases)?
    };

    let schema_key = schema
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    case_list.retain(|c| c.schema == schema_key);
    if case_list.is_empty() {
        anyhow::bail!(
            "no cases with schema '{}' (directory name must match case `schema:` field)",
            schema_key
        );
    }

    let pipeline = PromptPipelineConfig::for_cli_focus(cli.focus.as_deref()).with_render_mode(
        PromptRenderMode::parse_user_facing_or_default(cli.symbol_tuning.as_str()),
    );
    let prompt = pipeline.render_prompt(&cgs, None);
    let st = pipeline.prompt_surface_stats(&cgs, None, &prompt);
    let prompt_stats = PromptStatsSnapshot::from(st);
    eprintln!("schema prompt: {}", st.summary_line_body());

    let max_attempts = cli.attempts.max(1);

    if cli.dry_run {
        println!(
            "{}",
            serde_json::json!({
                "mode": "dry_run",
                "cases": case_list.len(),
                "prompt_chars": prompt_stats.prompt_chars,
                "prompt_token_est": prompt_stats.prompt_token_est,
                "prompt_token_est_div4": prompt_stats.prompt_token_est_div4,
                "json_tool_capability_count": prompt_stats.json_tool_capability_count,
                "json_tool_navigation_count": prompt_stats.json_tool_navigation_count,
                "json_tool_estimate": prompt_stats.json_tool_estimate,
                "max_attempts": max_attempts,
                "llm_mode": "single_multi_turn_transcript",
                "model": cli.model,
                "temperature": cli.temperature,
                "seed": cli.seed,
            })
        );
        return Ok(());
    }

    let api_key = std::env::var("OPENROUTER_API_KEY").context(
        "set OPENROUTER_API_KEY (plasm-eval loads `.env` from cwd if present) — required by BAML clients in baml_src/clients.baml",
    )?;

    plasm_eval::baml_client::init();

    // Runtime `openai-generic` → always OpenRouter; model id is the OpenRouter slug.
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(cli.model.as_str(), &api_key, cli.temperature, cli.seed),
    );
    registry.set_primary_client("EvalModel");
    eprintln!(
        "eval: model = {}, temperature = {}, seed = {}",
        cli.model, cli.temperature, cli.seed
    );

    eprintln!(
        "eval: {} cases, sequential transcript, {} attempt(s)/case (first user: teaching table+goal; later: `--- GOAL ---` only; assistant: Plasm `text` only — `attempt_trace` keeps full reasoning)",
        case_list.len(),
        max_attempts
    );

    let mut chat: Vec<PlanChatTurn> = Vec::new();
    let mut report: Vec<serde_json::Value> = Vec::with_capacity(case_list.len());
    for (idx, case) in case_list.iter().enumerate() {
        let v = run_one_case(
            &cgs,
            prompt.as_str(),
            case,
            max_attempts,
            &registry,
            &pipeline,
            &mut chat,
        )
        .map_err(|e| anyhow::anyhow!("case index {idx}: {e:#}"))?;
        report.push(v);
    }

    let human = format_eval_report(cli.model.as_str(), prompt.as_str(), &report);
    let envelope = serde_json::json!({
        "model": cli.model,
        "schema_prompt": prompt.as_str(),
        "schema_prompt_chars": prompt_stats.prompt_chars,
        "schema_prompt_token_est": prompt_stats.prompt_token_est,
        "schema_prompt_token_est_div4": prompt_stats.prompt_token_est_div4,
        "json_tool_capability_count": prompt_stats.json_tool_capability_count,
        "json_tool_navigation_count": prompt_stats.json_tool_navigation_count,
        "json_tool_estimate": prompt_stats.json_tool_estimate,
        "cases": report,
    });
    let json_pretty = serde_json::to_string_pretty(&envelope)?;
    eprint!("{}", human);

    let sidecar_dir = eval_cases_sidecar_dir(&cases);
    let slug = sanitize_model_slug(cli.model.as_str());
    write_latest_eval_artifacts(&sidecar_dir, cli.model.as_str(), &human, &json_pretty)?;
    eprintln!(
        "eval: reports → {}/{}.latest.{{human.txt,json}}",
        sidecar_dir.display(),
        slug
    );

    if let Some(dir) = &cli.report_dir {
        write_eval_report_artifacts(dir, &schema_key, cli.model.as_str(), &human, &json_pretty)?;
    }

    println!("{}", json_pretty);
    Ok(())
}

/// Directory where `{model}.latest.*` eval artifacts live: parent of a cases file, or the cases directory.
fn eval_cases_sidecar_dir(cases: &Path) -> PathBuf {
    if cases.is_dir() {
        cases.to_path_buf()
    } else {
        cases
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

fn write_latest_eval_artifacts(
    dir: &Path,
    model: &str,
    human: &str,
    json: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create eval sidecar dir {}", dir.display()))?;
    let slug = sanitize_model_slug(model);
    let h = dir.join(format!("{slug}.latest.human.txt"));
    let j = dir.join(format!("{slug}.latest.json"));
    std::fs::write(&h, human).with_context(|| format!("write {}", h.display()))?;
    std::fs::write(&j, json).with_context(|| format!("write {}", j.display()))?;
    Ok(())
}

fn sanitize_model_slug(model: &str) -> String {
    let mut s = String::with_capacity(model.len());
    for c in model.chars() {
        match c {
            '/' | '\\' | ':' | ' ' => s.push('-'),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' => s.push(c),
            _ => s.push('-'),
        }
    }
    let out = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn write_eval_report_artifacts(
    report_dir: &Path,
    schema_key: &str,
    model: &str,
    human: &str,
    json: &str,
) -> anyhow::Result<()> {
    let slug = sanitize_model_slug(model);
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let base = report_dir.join(schema_key);
    std::fs::create_dir_all(&base)
        .with_context(|| format!("create report dir {}", base.display()))?;
    let stem = format!("{slug}-{ts}");
    let human_path = base.join(format!("{stem}.human.txt"));
    let json_path = base.join(format!("{stem}.json"));
    std::fs::write(&human_path, human)
        .with_context(|| format!("write {}", human_path.display()))?;
    std::fs::write(&json_path, json).with_context(|| format!("write {}", json_path.display()))?;
    let latest_h = base.join(format!("{slug}.latest.human.txt"));
    let latest_j = base.join(format!("{slug}.latest.json"));
    std::fs::write(&latest_h, human).with_context(|| format!("write {}", latest_h.display()))?;
    std::fs::write(&latest_j, json).with_context(|| format!("write {}", latest_j.display()))?;
    eprintln!(
        "eval: reports → {} ({} + {}.latest.*)",
        base.display(),
        stem,
        slug
    );
    Ok(())
}

/// Schema prompt (full teaching table bundle) plus one-line summary and grouped failure blocks.
fn format_eval_report(model: &str, schema_prompt: &str, report: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "model: {}", model);
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "─".repeat(72));
    let _ = writeln!(
        out,
        "Plasm schema prompt (eval: teaching table only in the **first** user turn; later user turns are `--- GOAL ---` + goal; each assistant turn is the Plasm expression only, no reasoning — keeps requests small.)"
    );
    let _ = writeln!(out, "{}", "─".repeat(72));
    out.push_str(schema_prompt);
    if !schema_prompt.ends_with('\n') {
        out.push('\n');
    }
    let _ = writeln!(out, "{}", "─".repeat(72));
    let _ = writeln!(out);

    let n = report.len();
    if n == 0 {
        return out;
    }
    let mut sum = 0f64;
    let mut parse_ok = 0usize;
    let mut tc_ok = 0usize;
    for v in report {
        if let Some(s) = v.get("score").and_then(|x| x.as_f64()) {
            sum += s;
        }
        if v.get("parse_ok").and_then(|x| x.as_bool()) == Some(true) {
            parse_ok += 1;
        }
        if v.get("typecheck_ok").and_then(|x| x.as_bool()) == Some(true) {
            tc_ok += 1;
        }
    }
    let mean = sum / n as f64;
    let _ = writeln!(
        out,
        "eval summary: {} cases | mean score {:.4} | parse_ok {}/{} | typecheck_ok {}/{}",
        n, mean, parse_ok, n, tc_ok, n
    );

    let pipeline_hard: Vec<&serde_json::Value> = report
        .iter()
        .filter(|v| {
            v.get("parse_ok").and_then(|x| x.as_bool()) != Some(true)
                || v.get("typecheck_ok").and_then(|x| x.as_bool()) != Some(true)
        })
        .collect();

    let soft: Vec<&serde_json::Value> = report
        .iter()
        .filter(|v| {
            v.get("parse_ok").and_then(|x| x.as_bool()) == Some(true)
                && v.get("typecheck_ok").and_then(|x| x.as_bool()) == Some(true)
                && v.get("score")
                    .and_then(|x| x.as_f64())
                    .map(|s| s < 1.0 - f64::EPSILON)
                    .unwrap_or(true)
        })
        .collect();

    if pipeline_hard.is_empty() && soft.is_empty() {
        return out;
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "═".repeat(72));
    if !pipeline_hard.is_empty() {
        let _ = writeln!(
            out,
            "Pipeline failures (parse/typecheck never succeeded): {}",
            pipeline_hard.len()
        );
        let _ = writeln!(out);
        for v in &pipeline_hard {
            append_case_pipeline_failure(&mut out, v);
        }
    }
    if !soft.is_empty() {
        if !pipeline_hard.is_empty() {
            let _ = writeln!(out);
        }
        let _ = writeln!(
            out,
            "Expectation gaps (validated, soft score below 1.0): {}",
            soft.len()
        );
        let _ = writeln!(out);
        for v in &soft {
            append_case_expectation_gap(&mut out, v);
        }
    }
    let _ = writeln!(out, "{}", "═".repeat(72));
    let _ = writeln!(out);
    out
}

fn append_case_pipeline_failure(out: &mut String, v: &serde_json::Value) {
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?");
    let score = v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let sem = v
        .get("semantic_score")
        .and_then(|x| x.as_f64())
        .map(|s| format!("{s:.3}"))
        .unwrap_or_else(|| "—".to_string());
    let _ = writeln!(out, "▸ {}  final_score={:.4}  semantic={}", id, score, sem);
    if let Some(g) = v.get("goal").and_then(|x| x.as_str()) {
        if !g.is_empty() {
            let _ = writeln!(out, "  goal: {}", g);
        }
    }
    if let Some(re) = v.get("reference_expr").and_then(|x| x.as_str()) {
        if !re.is_empty() {
            let _ = writeln!(out, "  reference_expr: {}", re);
        }
    }
    if let Some(cm) = v.get("correction") {
        let rounds = cm.get("rounds_used").and_then(|x| x.as_u64()).unwrap_or(0);
        let recovered = cm
            .get("recovered")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let pipe = cm
            .get("pipeline_score")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        let _ = writeln!(
            out,
            "  rounds={}  recovered={}  pipeline_score={:.2}",
            rounds, recovered, pipe
        );
    }
    if let Some(trace) = v.get("attempt_trace").and_then(|x| x.as_array()) {
        if trace.is_empty() {
            let _ = writeln!(out, "  (no attempt_trace — BAML error before plan?)");
        }
        for att in trace {
            let n = att.get("attempt").and_then(|x| x.as_u64()).unwrap_or(0);
            let _ = writeln!(out, "  ─── attempt {} ───", n);
            if let Some(steps) = att.get("steps").and_then(|x| x.as_array()) {
                for (i, st) in steps.iter().enumerate() {
                    let text = st.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    let reason = st.get("reasoning").and_then(|x| x.as_str()).unwrap_or("");
                    let _ = writeln!(out, "    step {}: {}", i, text);
                    if !reason.is_empty() {
                        let _ = writeln!(out, "      reason: {}", reason);
                    }
                }
            }
            if let Some(ctx) = att.get("correction_context_in").and_then(|x| x.as_str()) {
                if !ctx.is_empty() {
                    let _ = writeln!(
                        out,
                        "    correction_context_in (feedback from prior round, sent to LLM):"
                    );
                    for line in format_truncated_multiline(ctx, 512, 48) {
                        let _ = writeln!(out, "      {}", line);
                    }
                }
            }
            if let Some(notes) = att.get("lexicon_notes").and_then(|x| x.as_array()) {
                if !notes.is_empty() {
                    let _ = writeln!(
                        out,
                        "    deterministic parse recovery (emitted → resolved):"
                    );
                    for n in notes {
                        let si = n.get("step_index").and_then(|x| x.as_u64()).unwrap_or(0);
                        let em = n.get("emitted").and_then(|x| x.as_str()).unwrap_or("");
                        let res = n.get("resolved_to").and_then(|x| x.as_str()).unwrap_or("");
                        let _ = writeln!(out, "      step {}: `{}` → `{}`", si, em, res);
                    }
                }
            }
            let ok = att
                .get("validation_ok")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if ok {
                let _ = writeln!(out, "    validation: ok");
            } else {
                let _ = writeln!(out, "    validation: FAILED");
                if let Some(diags) = att.get("diagnostics").and_then(|x| x.as_array()) {
                    for d in diags {
                        let _ = writeln!(out, "{}", format_step_diagnostic_pretty(d, "    "));
                    }
                }
            }
        }
    }
    let _ = writeln!(out);
}

fn append_case_expectation_gap(out: &mut String, v: &serde_json::Value) {
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?");
    let score = v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let sem = v
        .get("semantic_score")
        .and_then(|x| x.as_f64())
        .map(|s| format!("{s:.3}"))
        .unwrap_or_else(|| "—".to_string());
    let _ = writeln!(
        out,
        "▸ {}  blended_score={:.4}  semantic={}",
        id, score, sem
    );
    if let Some(g) = v.get("goal").and_then(|x| x.as_str()) {
        if !g.is_empty() {
            let _ = writeln!(out, "  goal: {}", g);
        }
    }
    if let Some(re) = v.get("reference_expr").and_then(|x| x.as_str()) {
        if !re.is_empty() {
            let _ = writeln!(out, "  reference_expr: {}", re);
        }
    }
    if let Some(notes) = v.get("notes").and_then(|x| x.as_array()) {
        if !notes.is_empty() {
            let _ = writeln!(out, "  expectation notes:");
            for n in notes {
                if let Some(s) = n.as_str() {
                    let _ = writeln!(out, "    - {}", s);
                }
            }
        }
    }
    if let Some(trace) = v.get("attempt_trace").and_then(|x| x.as_array()) {
        if let Some(last) = trace.last() {
            if let Some(steps) = last.get("steps").and_then(|x| x.as_array()) {
                let _ = writeln!(out, "  final plan (for context):");
                for (i, st) in steps.iter().enumerate() {
                    let text = st.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    let reason = st.get("reasoning").and_then(|x| x.as_str()).unwrap_or("");
                    let _ = writeln!(out, "    step {}: {}", i, text);
                    if !reason.is_empty() {
                        let _ = writeln!(out, "      reason: {}", reason);
                    }
                }
            }
        }
    }
    let _ = writeln!(out);
}

/// Pretty-print long JSON for stderr: cap lines and line length (wide enough for full hint lists).
fn format_truncated_multiline(s: &str, max_line_len: usize, max_lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = s.lines().collect();
    let total_lines = lines.len();
    for line in lines.iter().take(max_lines) {
        let t = if line.chars().count() > max_line_len {
            let mut acc = String::new();
            for (i, ch) in line.chars().enumerate() {
                if i >= max_line_len {
                    acc.push('…');
                    break;
                }
                acc.push(ch);
            }
            acc
        } else {
            (*line).to_string()
        };
        out.push(t);
    }
    if total_lines > max_lines {
        out.push(format!(
            "… ({} more line(s) in full correction_context)",
            total_lines - max_lines
        ));
    }
    out
}

fn format_step_diagnostic_pretty(d: &serde_json::Value, indent: &str) -> String {
    let step_index = d.get("step_index").and_then(|x| x.as_u64()).unwrap_or(0);
    let expr = d.get("expression").and_then(|x| x.as_str()).unwrap_or("");
    let err = d.get("error");
    let msg = err
        .and_then(|e| e.get("correction"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let cat = err
        .and_then(|e| e.get("category"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let mut out = format!(
        "{}  • [{}] step {} `{}`\n{}    {}",
        indent, cat, step_index, expr, indent, msg
    );
    if let Some(off) = err
        .and_then(|e| e.get("span_offset"))
        .and_then(|x| x.as_u64())
    {
        out.push_str(&format!("\n{}    span_offset: {}", indent, off));
    }
    out
}
