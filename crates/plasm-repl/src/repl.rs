//! Local Python DAG authoring. Source is buffered losslessly and compiled by the host.
use anyhow::Context;
use plasm_agent::output::{format_result_with_cgs, OutputFormat};
use plasm_agent::{
    http::{build_plasm_host_state, PlasmHostBootstrap},
    server_state::{CatalogBootstrap, PlasmHostState},
    PlasmCompBundle,
};
use plasm_core::{CgsRegistry, CGS};
use plasm_eval::baml_client::types::{PlanChatTurn, Union2KassistantOrKuser};
use plasm_eval::baml_client::{sync_client::B, ClientRegistry};
use plasm_eval::{
    build_correction_feedback, nl_translate_user_bundle, openrouter_eval_llm_options,
    validate_programs, ProgramSession, DEFAULT_OPENROUTER_EVAL_SEED,
    DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
};
use plasm_runtime::{ExecutionEngine, ExecutionMode, SessionMaterialization};
use rustyline::{error::ReadlineError, DefaultEditor};
use std::sync::Arc;

#[derive(Clone, Default)]
struct LlmState {
    model: Option<String>,
    attempts: u32,
    chat: Vec<PlanChatTurn>,
}

pub async fn run_repl(
    cgs: &CGS,
    engine: ExecutionEngine,
    mode: ExecutionMode,
    mut format: OutputFormat,
    focus: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Arc::new(ProgramSession::new(cgs, focus.as_deref())?);
    let registry = CgsRegistry::from_pairs(vec![(
        "local".into(),
        "Local".into(),
        vec![],
        session.execute.cgs.clone(),
    )]);
    let mut host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode,
        registry: Arc::new(registry),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    println!(
        "{}\nEnter a Python Program, then :plan or :run. :help lists commands.",
        session.prompt()
    );
    let mut editor = DefaultEditor::new()?;
    let mut source = String::new();
    let mut llm = LlmState {
        attempts: 2,
        ..Default::default()
    };
    loop {
        let line = match editor.readline(if source.is_empty() { "plasm> " } else { "... " }) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                source.clear();
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        let _ = editor.add_history_entry(&line);
        if line.starts_with(':') {
            let (command, arg) = line.split_once(' ').unwrap_or((&line, ""));
            match command {
                ":quit" | ":exit" | ":q" => break,
                ":help" | ":?" => print_help(),
                ":reset" => source.clear(),
                ":show" => print!("{source}"),
                ":load" => match std::fs::read_to_string(arg.trim()) {
                    Ok(text) => source = text,
                    Err(error) => eprintln!("{error}"),
                },
                ":schema" => {
                    if arg.trim().is_empty() {
                        println!("{}", session.prompt());
                    } else {
                        match Arc::get_mut(&mut session)
                            .expect("no compiler in flight")
                            .extend(arg.trim())
                        {
                            Ok(delta) => {
                                println!("{delta}");
                                if !delta.is_empty() && !llm.chat.is_empty() {
                                    llm.chat.push(PlanChatTurn {
                                        role: Union2KassistantOrKuser::Kuser,
                                        content: delta,
                                    });
                                }
                            }
                            Err(error) => eprintln!("{error}"),
                        }
                    }
                }
                ":clear" => {
                    *session.execute.graph_cache.lock().await = SessionMaterialization::new();
                    println!("Materialized cache cleared; symbols retained.");
                }
                ":mode" => match arg.trim() {
                    "live" => host.oss.mode = ExecutionMode::Live,
                    "replay" => host.oss.mode = ExecutionMode::Replay,
                    "hybrid" => host.oss.mode = ExecutionMode::Hybrid,
                    _ => eprintln!("Use :mode live|replay|hybrid"),
                },
                ":output" => match arg.trim() {
                    "json" | "table" | "compact" => format = OutputFormat::parse(arg.trim()),
                    _ => eprintln!("Use :output json|table|compact"),
                },
                ":llm" => {
                    let arg = arg.trim();
                    if arg == "off" {
                        llm.model = None;
                        llm.chat.clear();
                    } else if let Some(count) = arg.strip_prefix("attempts ") {
                        match count.parse::<u32>() {
                            Ok(n) if n > 0 => llm.attempts = n,
                            _ => eprintln!("attempts must be positive"),
                        }
                    } else if !arg.is_empty() {
                        llm.model = Some(arg.into());
                        llm.chat.clear();
                    } else {
                        eprintln!("Use :llm MODEL | :llm off | :llm attempts N");
                    }
                }
                ":plan" | ":run" => match session.compile(&source) {
                    Ok(bundle) => {
                        if execute(&session, &host, &bundle, command == ":run", format).await {
                            source.clear();
                        }
                    }
                    Err(error) => eprintln!("{}", error.agent_markdown()),
                },
                _ => eprintln!("Unknown command; use :help"),
            }
            continue;
        }
        if llm.model.is_some() && source.is_empty() && !line.trim().is_empty() {
            let compile_session = session.clone();
            let state = llm.clone();
            match tokio::task::spawn_blocking(move || translate(&compile_session, &state, &line))
                .await?
            {
                Ok((text, turns)) => {
                    println!("{text}\nUse :plan or :run.");
                    source = text;
                    llm.chat.extend(turns);
                }
                Err(error) => eprintln!("{error:#}"),
            }
        } else {
            source.push_str(&line);
            source.push('\n');
        }
    }
    Ok(())
}

async fn execute(
    session: &ProgramSession,
    host: &PlasmHostState,
    bundle: &PlasmCompBundle,
    run: bool,
    format: OutputFormat,
) -> bool {
    match plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &session.execute,
        host,
        &session.execute.prompt_hash,
        "local",
        bundle,
        run,
        None,
        None,
        None,
        None,
    )
    .await
    {
        Ok(result) => {
            if run {
                for step in result.return_steps {
                    let (text, omitted, _) =
                        format_result_with_cgs(&step.result, format, step.cgs.as_deref());
                    println!("{text}");
                    if !omitted.is_empty() {
                        println!("Omitted from summary: {}", omitted.join(", "));
                    }
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result.node_results).expect("plan JSON")
                );
            }
            run
        }
        Err(error) => {
            eprintln!("{error}\nInspect completed-write evidence before retrying effects.");
            false
        }
    }
}

fn translate(
    session: &ProgramSession,
    state: &LlmState,
    goal: &str,
) -> anyhow::Result<(String, Vec<PlanChatTurn>)> {
    let key = std::env::var("OPENROUTER_API_KEY").context("set OPENROUTER_API_KEY for :llm")?;
    plasm_eval::baml_client::init();
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(
            state.model.as_deref().context("model")?,
            &key,
            DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
            DEFAULT_OPENROUTER_EVAL_SEED,
        ),
    );
    registry.set_primary_client("EvalModel");
    let mut feedback = String::new();
    let mut turns = Vec::new();
    for attempt in 0..state.attempts {
        let content = nl_translate_user_bundle(
            state.chat.is_empty() && turns.is_empty(),
            session.prompt(),
            goal,
            &feedback,
        );
        turns.push(PlanChatTurn {
            role: Union2KassistantOrKuser::Kuser,
            content,
        });
        let mut messages = state.chat.clone();
        messages.extend(turns.clone());
        let plan = B
            .TranslatePlan
            .with_client_registry(&registry)
            .call(&messages)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        turns.push(PlanChatTurn {
            role: Union2KassistantOrKuser::Kassistant,
            content: plan.text.clone(),
        });
        match validate_programs(session, std::slice::from_ref(&plan.text)) {
            Ok(_) => return Ok((plan.text, turns)),
            Err(diagnostics) => {
                feedback = build_correction_feedback(
                    goal,
                    attempt as usize,
                    &[(plan.text.as_str(), plan.reasoning.as_str())],
                    &diagnostics,
                )
            }
        }
    }
    anyhow::bail!("Python program still needs correction: {feedback}")
}

fn print_help() {
    println!("Python DAG REPL\nPaste one class derived from Program, preserving indentation. Finish with :plan or :run.\n:plan                  compile and review buffered DAG\n:run                   compile and execute buffered DAG\n:load PATH             replace buffer with a Python file\n:show / :reset         inspect / discard buffered source\n:schema [Entity]       show teaching / extend domain declarations\n:clear                 clear materialized rows; retain symbols\n:mode live|replay|hybrid\n:output json|table|compact\n:llm MODEL | :llm off | :llm attempts N\n:quit\n\nThe outer Program declares a DAG. Deferred formatting uses @compute. No native Plasm syntax is accepted.");
}
