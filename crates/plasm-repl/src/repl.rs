//! Interactive REPL for the Plasm path expression language.
//!
//! Run the **`plasm-repl`** binary: `plasm-repl --schema <dir-or-yaml> --backend <url>` (or `--catalog-dir` instead of `--schema`).
//!
//! On startup:
//!   - Renders the same teaching prompt as `plasm-eval` / BAML `TranslatePlan` (one expression per goal)
//!     (see [`plasm_core::PromptPipelineConfig`] on the execution engine). In `:llm` mode the schema prompt is
//!     sent only for the first NL line of a session; later lines reuse chat history (multi-turn).
//!   - Enters a readline loop that accepts Plasm path expressions.
//!
//! REPL commands (prefixed with `:`):
//!   :help / :?         show grammar reference
//!   :schema [entity]   re-render the entity catalog (optionally focused on entity)
//!   :clear             clear the graph cache
//!   :mode live|replay  toggle execution mode
//!   :output json|table|compact  toggle output format
//!   :llm <model>|off|attempts N   natural-language mode via OpenRouter (needs OPENROUTER_API_KEY)
//!   :quit / :exit / Ctrl-D  exit

use std::sync::Arc;

use anyhow::Context;
use plasm_core::{
    domain_lexicon::DomainLexicon,
    error_render::{self, format_recovery_hints, FeedbackStyle},
    expr_correction::parse_session_line_with_rewrite_recovery,
    expr_parser::ParsedExpr,
    normalize_expr_query_capabilities, symbol_map_for_prompt, PromptPipelineConfig, CGS,
};
use plasm_eval::baml_client::types::{PlanChatTurn, Union2KassistantOrKuser};
use plasm_eval::baml_client::{sync_client::B, ClientRegistry};
use plasm_eval::{
    build_correction_feedback, nl_translate_user_bundle, openrouter_eval_llm_options,
    validate_plan_steps_with_lexicon_detailed, DEFAULT_OPENROUTER_EVAL_SEED,
    DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
};
use plasm_runtime::{
    ExecuteOptions, ExecutionEngine, ExecutionMode, SessionMaterialization, StreamConsumeOpts,
};
use rustyline::{error::ReadlineError, DefaultEditor};
use tracing::Instrument;

use plasm_agent::output::{format_result_with_cgs, OutputFormat};

const PROMPT: &str = "plasm> ";

fn eprint_schema_prompt_stats(
    cgs: &CGS,
    pipeline: &PromptPipelineConfig,
    prompt_focus: Option<&str>,
    schema_text: &str,
) {
    let s = pipeline.prompt_surface_stats(cgs, prompt_focus, schema_text);
    eprintln!("schema prompt: {}", s.summary_line_body());
}

/// OpenRouter model id and correction settings for NL → `TranslatePlan` in the REPL.
#[derive(Debug, Clone)]
struct LlmState {
    enabled: bool,
    model: String,
    attempts: u32,
    /// Prior user/assistant turns for multi-turn `TranslatePlan` (schema only in the first user turn).
    nl_chat: Vec<PlanChatTurn>,
}

impl Default for LlmState {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            attempts: 2,
            nl_chat: Vec::new(),
        }
    }
}

pub async fn run_repl(
    cgs: &CGS,
    engine: &ExecutionEngine,
    initial_mode: ExecutionMode,
    initial_format: OutputFormat,
    mut prompt_focus: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    plasm_agent::dotenv_safe::load_from_cwd_parents();

    let pipeline = engine.prompt_pipeline();
    let schema_text = pipeline.render_prompt(cgs, prompt_focus.as_deref());
    eprint_schema_prompt_stats(cgs, pipeline, prompt_focus.as_deref(), &schema_text);
    println!("{schema_text}");
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Plasm REPL  |  :help for commands  |  Ctrl-D to exit");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let mut rl = DefaultEditor::new()?;
    let mut mat = SessionMaterialization::new();
    let mut mode = initial_mode;
    let mut output_format = initial_format;
    let lexicon = DomainLexicon::from_cgs(cgs);
    let mut llm = LlmState::default();
    let cgs = Arc::new(cgs.clone());

    loop {
        let readline = rl.readline(PROMPT);
        match readline {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);

                if line.starts_with(':') {
                    if handle_command(
                        &line,
                        cgs.as_ref(),
                        engine,
                        &mut mode,
                        &mut output_format,
                        &mut mat,
                        &mut prompt_focus,
                        &mut llm,
                    ) {
                        break;
                    }
                    continue;
                }

                if llm.enabled {
                    let goal = line.clone();
                    let schema_context = if llm.nl_chat.is_empty() {
                        let p = engine.prompt_pipeline();
                        let s = p.render_prompt(cgs.as_ref(), prompt_focus.as_deref());
                        eprint_schema_prompt_stats(cgs.as_ref(), p, prompt_focus.as_deref(), &s);
                        s
                    } else {
                        String::new()
                    };
                    let prior_chat = llm.nl_chat.clone();
                    let model = llm.model.clone();
                    let max_attempts = llm.attempts.max(1);
                    let cgs_block = Arc::clone(&cgs);
                    let focus_for_nl = prompt_focus.clone();
                    let pipeline = engine.prompt_pipeline().clone();
                    let translate = tokio::task::spawn_blocking(move || {
                        translate_nl_goal(
                            cgs_block.as_ref(),
                            &prior_chat,
                            &schema_context,
                            &goal,
                            &model,
                            max_attempts,
                            focus_for_nl.as_deref(),
                            &pipeline,
                        )
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("join: {e}"))?;

                    match translate {
                        Ok((steps, new_turns)) => {
                            llm.nl_chat.extend(new_turns);
                            for (si, (parsed, reasoning)) in steps.into_iter().enumerate() {
                                if !reasoning.trim().is_empty() {
                                    eprintln!(
                                        "\x1b[2m[llm step {}] {}\x1b[0m",
                                        si + 1,
                                        reasoning.trim()
                                    );
                                }
                                execute_parsed_expr(
                                    line.as_str(),
                                    parsed,
                                    cgs.as_ref(),
                                    engine,
                                    &mut mat,
                                    mode,
                                    output_format,
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            eprintln!("\x1b[31mllm translate error:\x1b[0m {e:#}");
                        }
                    }
                    println!();
                    continue;
                }

                // Parse and execute (in-grammar symbol resolution; no string pre-expansion)
                let pipeline = engine.prompt_pipeline();
                let symbol_map = pipeline.with_focus_spec(prompt_focus.as_deref(), |focus| {
                    symbol_map_for_prompt(cgs.as_ref(), focus, pipeline.uses_symbols())
                });
                match parse_session_line_with_rewrite_recovery(
                    line.trim(),
                    cgs.as_ref(),
                    &lexicon,
                    symbol_map,
                ) {
                    Err((e, work, extra_hints)) => {
                        let sym_map = pipeline.with_focus_spec(prompt_focus.as_deref(), |focus| {
                            symbol_map_for_prompt(cgs.as_ref(), focus, pipeline.uses_symbols())
                        });
                        let feedback = match sym_map.as_deref() {
                            Some(m) => FeedbackStyle::SymbolicLlm { map: m },
                            None => FeedbackStyle::CanonicalDev,
                        };
                        let correction_head = match sym_map.as_deref() {
                            Some(_) => line.as_str(),
                            None => work.as_str(),
                        };
                        let mut step_err = error_render::render_parse_error_with_feedback(
                            &e,
                            &work,
                            line.as_str(),
                            cgs.as_ref(),
                            feedback.clone(),
                        );
                        if !extra_hints.is_empty() {
                            let merged = format_recovery_hints(&extra_hints, feedback.clone());
                            step_err.correction = format!("{correction_head}\n\n{}", merged);
                        }
                        if let Some(ref d) = step_err.error {
                            eprintln!("\x1b[2m{}\x1b[0m", d);
                        }
                        eprintln!("\x1b[33mcorrection:\x1b[0m {}", step_err.correction);
                    }
                    Ok((parsed, _)) => {
                        execute_parsed_expr(
                            line.as_str(),
                            parsed,
                            cgs.as_ref(),
                            engine,
                            &mut mat,
                            mode,
                            output_format,
                        )
                        .await;
                    }
                }
                println!();
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: clear line, continue
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D: exit
                println!("Goodbye.");
                break;
            }
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    Ok(())
}

type NlTranslateOutcome = (Vec<(ParsedExpr, String)>, Vec<PlanChatTurn>);

/// Run `TranslatePlan` + validation (same pipeline as plasm-eval), on a worker thread.
///
/// `prior_chat` holds completed turns. When empty, `schema_context` must be the full eval bundle;
/// otherwise the latest user message omits the schema (it remains in earlier transcript turns).
/// On success, returns `(steps, new_turns)` to append to the REPL session (`user` then `assistant`).
#[allow(clippy::too_many_arguments)]
fn translate_nl_goal(
    cgs: &CGS,
    prior_chat: &[PlanChatTurn],
    schema_context: &str,
    goal: &str,
    model: &str,
    max_attempts: u32,
    prompt_focus: Option<&str>,
    pipeline: &PromptPipelineConfig,
) -> anyhow::Result<NlTranslateOutcome> {
    let api_key = std::env::var("OPENROUTER_API_KEY").context(
        "set OPENROUTER_API_KEY for LLM mode (optional `.env` in cwd is loaded on REPL start)",
    )?;

    plasm_eval::baml_client::init();

    // `openai-generic` → OpenRouter only; `model` is the OpenRouter model id.
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(
            model,
            &api_key,
            DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
            DEFAULT_OPENROUTER_EVAL_SEED,
        ),
    );
    registry.set_primary_client("EvalModel");
    let registry = Arc::new(registry);

    let mut correction_context = String::new();
    let lexicon = DomainLexicon::from_cgs(cgs);
    let first_turn = prior_chat.is_empty();

    for attempt in 0..max_attempts {
        let user_content = nl_translate_user_bundle(
            first_turn,
            schema_context,
            goal,
            correction_context.as_str(),
        );
        let mut messages: Vec<PlanChatTurn> = prior_chat.to_vec();
        messages.push(PlanChatTurn {
            role: Union2KassistantOrKuser::Kuser,
            content: user_content,
        });

        let plan = B
            .TranslatePlan
            .with_client_registry(registry.as_ref())
            .call(&messages)
            .map_err(|e| anyhow::anyhow!("BAML TranslatePlan: {e}"))?;

        // Eval and REPL accept a single expression only (see baml_src/query_expr.baml).
        let text = plan.text.trim().to_string();
        let reasoning = plan.reasoning.trim().to_string();
        let texts = vec![text.clone()];
        let step_pairs: Vec<(&str, &str)> = vec![(text.as_str(), reasoning.as_str())];

        let (validation, _lexicon_notes) = validate_plan_steps_with_lexicon_detailed(
            cgs,
            &texts,
            &lexicon,
            pipeline,
            prompt_focus,
        );
        match validation {
            Ok(parsed) => {
                let expr = parsed
                    .into_iter()
                    .next()
                    .expect("single expression from TranslatePlan");
                let user_hist = if first_turn {
                    format!("{schema_context}\n--- GOAL ---\n{goal}")
                } else {
                    format!("--- GOAL ---\n{goal}")
                };
                let new_turns = vec![
                    PlanChatTurn {
                        role: Union2KassistantOrKuser::Kuser,
                        content: user_hist,
                    },
                    PlanChatTurn {
                        role: Union2KassistantOrKuser::Kassistant,
                        content: format!("Plasm expression: `{text}`\n{reasoning}"),
                    },
                ];
                return Ok((vec![(expr, reasoning)], new_turns));
            }
            Err(diags) => {
                correction_context =
                    build_correction_feedback(goal, attempt as usize, &step_pairs, &diags);
                if attempt + 1 == max_attempts {
                    let json = serde_json::to_string_pretty(&diags)
                        .unwrap_or_else(|_| format!("{diags:?}"));
                    anyhow::bail!(
                        "plan validation failed after {max_attempts} attempt(s):\n{json}"
                    );
                }
            }
        }
    }

    anyhow::bail!("internal: exhausted attempts without result");
}

async fn execute_parsed_expr(
    source_line: &str,
    mut parsed: ParsedExpr,
    cgs: &CGS,
    engine: &ExecutionEngine,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    output_format: OutputFormat,
) {
    if let Err(e) = normalize_expr_query_capabilities(&mut parsed.expr, cgs) {
        eprintln!("\x1b[31mquery resolve error:\x1b[0m {e}");
        return;
    }
    println!(
        "\x1b[2m→ {}\x1b[0m",
        plasm_agent::expr_display::expr_display(&parsed.expr)
    );
    if let Some(ref proj) = parsed.projection {
        println!("\x1b[2m  projection: [{}]\x1b[0m", proj.join(", "));
    }

    let mut log_expr = format!(
        "→ {}",
        plasm_agent::expr_display::expr_display(&parsed.expr)
    );
    if let Some(ref proj) = parsed.projection {
        if !proj.is_empty() {
            log_expr.push_str(&format!("\n  projection: [{}]", proj.join(", ")));
        }
    }
    let expr_span =
        plasm_agent::spans::execute_expression_line("repl", source_line.len(), log_expr.len());
    expr_span.in_scope(|| {
        tracing::trace!(
            source_expression = %source_line,
            parsed_expression = %log_expr,
            "execute expression"
        );
    });

    match engine
        .execute(
            &parsed.expr,
            cgs,
            cache,
            Some(mode),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .instrument(expr_span.clone())
        .await
    {
        Ok(mut result) => {
            if let Some(ref fields) = parsed.projection {
                if !result.entities.is_empty() {
                    let entity_type = result.entities[0].reference.entity_type.clone();
                    match engine
                        .auto_resolve_projection(
                            result.entities.clone(),
                            &entity_type,
                            fields,
                            cgs,
                            cache,
                            mode,
                            ExecuteOptions::default(),
                        )
                        .instrument(expr_span.clone())
                        .await
                    {
                        Ok(enriched) => {
                            result.entities = enriched;
                            result.count = result.entities.len();
                        }
                        Err(e) => {
                            eprintln!("auto-resolve warning: {}", e);
                        }
                    }
                }
                plasm_agent::output::apply_projection(&mut result, fields);
            }
            let (formatted, omitted, _fidelity) =
                format_result_with_cgs(&result, output_format, Some(cgs));
            println!("{}", formatted);
            if !omitted.is_empty() {
                println!("(omitted from summary: {})", omitted.join(", "));
            }
            let net = result.stats.network_requests;
            let hits = result.stats.cache_hits;
            let mut parts: Vec<String> = Vec::new();
            if net > 0 {
                parts.push(format!(
                    "{} http call{}",
                    net,
                    if net == 1 { "" } else { "s" }
                ));
            }
            if hits > 0 {
                parts.push(format!(
                    "{} cache hit{}",
                    hits,
                    if hits == 1 { "" } else { "s" }
                ));
            }
            let stats_str = if parts.is_empty() {
                String::new()
            } else {
                let joined = parts.join(", ");
                format!(", {joined}")
            };
            println!(
                "\x1b[2m({} result{}, {:?}, {}ms{})\x1b[0m",
                result.count,
                if result.count == 1 { "" } else { "s" },
                result.source,
                result.stats.duration_ms,
                stats_str,
            );
        }
        Err(e) => {
            eprintln!("\x1b[31mexec error:\x1b[0m {e}");
        }
    }
}

/// Returns `true` if the REPL should exit.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    line: &str,
    cgs: &CGS,
    engine: &ExecutionEngine,
    mode: &mut ExecutionMode,
    format: &mut OutputFormat,
    cache: &mut SessionMaterialization,
    prompt_focus: &mut Option<String>,
    llm: &mut LlmState,
) -> bool {
    if line.starts_with(":llm") {
        handle_llm_command(line, llm);
        return false;
    }

    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    match parts[0] {
        ":help" | ":?" => {
            print_help();
        }
        ":schema" => {
            let focus_before = prompt_focus.clone();
            let focus_for_render: Option<String> = match parts.get(1).copied() {
                Some(f) if !f.is_empty() => {
                    if cgs.get_entity(f).is_none() {
                        eprintln!("unknown entity '{f}'");
                        return false;
                    }
                    *prompt_focus = Some(f.to_string());
                    Some(f.to_string())
                }
                _ => prompt_focus.clone(),
            };
            if llm.enabled && focus_before != *prompt_focus {
                llm.nl_chat.clear();
            }
            let p = engine.prompt_pipeline();
            let text = p.render_prompt(cgs, focus_for_render.as_deref());
            eprint_schema_prompt_stats(cgs, p, focus_for_render.as_deref(), &text);
            println!("{text}");
        }
        ":clear" => {
            *cache = SessionMaterialization::new();
            println!("cache cleared.");
        }
        ":mode" => match parts.get(1).copied() {
            Some("live") => {
                *mode = ExecutionMode::Live;
                println!("mode: live");
            }
            Some("replay") => {
                *mode = ExecutionMode::Replay;
                println!("mode: replay");
            }
            Some("hybrid") => {
                *mode = ExecutionMode::Hybrid;
                println!("mode: hybrid");
            }
            other => {
                eprintln!("unknown mode {:?}. Use: live | replay | hybrid", other);
            }
        },
        ":output" => match parts.get(1).copied() {
            Some("json") => {
                *format = OutputFormat::Json;
                println!("output: json");
            }
            Some("table") => {
                *format = OutputFormat::Table;
                println!("output: table");
            }
            Some("compact") => {
                *format = OutputFormat::Compact;
                println!("output: compact");
            }
            other => {
                eprintln!("unknown format {:?}. Use: json | table | compact", other);
            }
        },
        ":quit" | ":exit" | ":q" => {
            println!("Goodbye.");
            return true;
        }
        other => {
            eprintln!("unknown command '{other}'. Type :help for available commands.");
        }
    }
    false
}

fn handle_llm_command(line: &str, llm: &mut LlmState) {
    let rest = line.strip_prefix(":llm").unwrap_or("").trim();
    if rest.is_empty() {
        print_llm_help();
        return;
    }
    if rest == "off" {
        llm.enabled = false;
        llm.nl_chat.clear();
        println!("LLM mode: off (lines are parsed as Plasm expressions).");
        return;
    }
    if let Some(n_str) = rest.strip_prefix("attempts") {
        let n_str = n_str.trim();
        match n_str.parse::<u32>() {
            Ok(n) if n >= 1 => {
                llm.attempts = n;
                println!("LLM correction attempts per goal: {n}");
            }
            _ => eprintln!("usage: :llm attempts <n>   (n >= 1)"),
        }
        return;
    }

    match std::env::var("OPENROUTER_API_KEY") {
        Ok(_) => {}
        Err(_) => {
            eprintln!(
                "LLM mode requires OPENROUTER_API_KEY (set in the environment or `.env` in cwd)."
            );
            return;
        }
    }

    llm.model = rest.to_string();
    llm.enabled = true;
    llm.nl_chat.clear();
    println!(
        "LLM mode: on (model `{}`, {} correction attempt(s) per goal). Lines are natural-language goals; later lines keep a transcript (schema sent once per session).",
        llm.model,
        llm.attempts.max(1)
    );
}

fn print_llm_help() {
    println!(
        r#"
LLM mode (OpenRouter via BAML `TranslatePlan`, same pipeline as plasm-eval):
  :llm <openrouter-model-id>   enable — e.g. :llm anthropic/claude-3.5-haiku
  :llm off                     disable (default): input is Plasm syntax
  :llm attempts <n>            correction rounds per goal (default 2, min 1)
Requires OPENROUTER_API_KEY. First NL line sends the full schema (eval bundle); follow-ups reuse chat history. Changing :schema focus or re-:llm clears the session.
"#
    );
}

fn print_help() {
    println!(
        r#"
REPL COMMANDS:
  :help / :?             show this help
  :schema [Entity]       re-display schema (optionally focused on Entity + neighbours)
  :clear                 clear the graph cache
  :mode live|replay|hybrid  switch execution mode
  :output json|table|compact  switch output format
  :llm                   help for natural-language mode (OpenRouter)
  :quit / :exit / :q     exit (also Ctrl-D)

EXPRESSION SYNTAX:
  Entity(id)              get by ID
  Entity(k=v,k2=v2,...)   get by compound key (when SCHEMA lists key_vars)
  Entity{{f=v,f>v,...}}    query with filters (= != > < >= <= ~ for contains)
  Entity                  query all
  Entity~"text"           full-text search only for entities that expose Search in teaching table (~ line)
  .field                  follow EntityRef field → target entity
  .^Entity                reverse: all Entity referencing this via FK
  .^Entity{{preds}}        reverse with filters
  [f,f,...]               select specific fields (append to any expression)
  foreign.field=val       cross-entity filter (e.g. pet.status=available)

EXAMPLES:
  Pet(10)
  Issue(owner=$,repo=$,number=$)
  Pet{{status=available}}[name,id]
  Order(5).petId
  Pet(10).^Order{{quantity>2}}
"#
    );
}
