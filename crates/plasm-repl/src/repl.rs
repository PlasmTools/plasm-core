//! Local Python DAG authoring. Source is buffered losslessly and compiled by the host.
use plasm_agent::output::{format_result_with_cgs, OutputFormat};
use plasm_agent::{
    http::{build_plasm_host_state, PlasmHostBootstrap},
    server_state::{CatalogBootstrap, PlasmHostState},
    PlasmCompBundle,
};
use plasm_core::{CgsRegistry, CGS};
use plasm_eval::baml_client::types::{PlanChatTurn, Union2KassistantOrKuser};
use plasm_eval::baml_client::{async_client::B, ClientRegistry};
use plasm_eval::{
    build_correction_feedback, nl_translate_user_bundle, openrouter_eval_llm_options,
    validate_programs, ProgramSession, DEFAULT_OPENROUTER_EVAL_SEED,
    DEFAULT_OPENROUTER_EVAL_TEMPERATURE,
};
use plasm_runtime::{ExecutionEngine, ExecutionMode, SessionMaterialization};
use rustyline::{error::ReadlineError, DefaultEditor};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
enum TranslateError {
    #[error("OPENROUTER_API_KEY is required for `:llm`")]
    ApiKeyMissing,
    #[error("select a model before invoking `:llm`")]
    ModelMissing,
    #[error("BAML TranslatePlan call failed: {0}")]
    Client(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("program validation host failed: {0}")]
    Host(#[source] plasm_agent::compilation_error::ExecutionFailure),
    #[error("Python program still needs correction: {feedback}")]
    CorrectionsExhausted { feedback: String },
}

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
    })?;
    let training = match Box::pin(ReplTraining::initialize(
        &mut host,
        Arc::get_mut(&mut session).expect("new session"),
    ))
    .await
    {
        Ok(training) => training,
        Err(error) => {
            eprintln!("Training unavailable: {error}; ordinary authoring continues.");
            None
        }
    };
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
                ":training" => match &training {
                    Some(training) => if let Err(error) = Box::pin(training.command(&host, &session, arg.trim())).await { eprintln!("Training: {error}"); },
                    None => eprintln!("Training unavailable: configure trusted host PLASM_REPL_TRAINING_SUBJECT, PLASM_REPL_TRAINING_CREDENTIAL, PLASM_REPL_TRAINING_TENANT, PLASM_REPL_TRAINING_PROJECT and PLASM_WORKFLOW_DATABASE_URL before starting the REPL."),
                },
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
                ":plan" | ":run" => match session.compile(&source).await {
                    Ok(bundle) => {
                        if Box::pin(execute(&session, &host, &bundle, command == ":run", format, training.as_ref())).await {
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
            match translate(&compile_session, &state, &line).await {
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
    training: Option<&ReplTraining>,
) -> bool {
    if let Some(training) = training {
        if let Err(error) = Box::pin(training.publish(host, session)).await {
            eprintln!("Training exposure unavailable: {error}; ordinary execution continues.");
        }
    }
    let execute_id = training
        .map(|training| training.session_id.as_str())
        .unwrap_or("local");
    let execution = Box::pin(plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &session.execute,
        host,
        &session.execute.prompt_hash,
        execute_id,
        bundle,
        run,
        None,
        None,
        None,
        None,
    ));
    let result = if run && training.is_some() {
        Box::pin(plasm_agent::workflow_learning::native::observe_execution(
            host,
            &session.execute,
            execute_id,
            bundle,
            execution,
        ))
        .await
    } else {
        execution.await
    };
    match result {
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

async fn translate(
    session: &ProgramSession,
    state: &LlmState,
    goal: &str,
) -> Result<(String, Vec<PlanChatTurn>), TranslateError> {
    let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| TranslateError::ApiKeyMissing)?;
    plasm_eval::baml_client::init();
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(
            state.model.as_deref().ok_or(TranslateError::ModelMissing)?,
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
            .await
            .map_err(|error| TranslateError::Client(Box::new(error)))?;
        turns.push(PlanChatTurn {
            role: Union2KassistantOrKuser::Kassistant,
            content: plan.text.clone(),
        });
        match validate_programs(session, std::slice::from_ref(&plan.text)).await {
            Ok(_) => return Ok((plan.text, turns)),
            Err(plasm_eval::ValidationFailure::Host(error)) => {
                return Err(TranslateError::Host(error))
            }
            Err(plasm_eval::ValidationFailure::Program(diagnostics)) => {
                feedback = build_correction_feedback(
                    goal,
                    attempt as usize,
                    &[(plan.text.as_str(), plan.reasoning.as_str())],
                    &diagnostics,
                )
            }
        }
    }
    Err(TranslateError::CorrectionsExhausted { feedback })
}

fn print_help() {
    println!("Python DAG REPL\nPaste one class derived from Program, preserving indentation. Finish with :plan or :run.\n:plan                  compile and review buffered DAG\n:run                   compile and execute buffered DAG\n:load PATH             replace buffer with a Python file\n:show / :reset         inspect / discard buffered source\n:schema [Entity]       show teaching / extend domain declarations\n:clear                 clear materialized rows; retain symbols\n:mode live|replay|hybrid\n:output json|table|compact\n:llm MODEL | :llm off | :llm attempts N\n:training status | runs | candidates | command FILE | review FILE | candidate REVIEW.HASH\n:quit\n\nThe outer Program declares a DAG. Deferred formatting uses @compute. No native Plasm syntax is accepted.");
}

/// The process host supplies identity. REPL text cannot create human authority.
struct ReplTraining {
    config: plasm_agent::workflow_learning::native::NativeLearningConfig,
    actor: plasm_agent::workflow_learning::training::TrustedTrainingActor,
    logical: plasm_agent::session_identity::LogicalSessionId,
    session_id: String,
    turns: Vec<String>,
}

impl ReplTraining {
    async fn initialize(
        host: &mut PlasmHostState,
        session: &mut ProgramSession,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        use plasm_agent::workflow_learning::{
            model::LibraryScope,
            native::{NativeLearningConfig, NativeLibraryConfig},
            training::TrustedTrainingActor,
        };
        let names = [
            "PLASM_REPL_TRAINING_SUBJECT",
            "PLASM_REPL_TRAINING_CREDENTIAL",
            "PLASM_REPL_TRAINING_TENANT",
            "PLASM_REPL_TRAINING_PROJECT",
            "PLASM_WORKFLOW_DATABASE_URL",
        ];
        let values: Vec<_> = names
            .iter()
            .map(|name| {
                std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .collect();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let values = values
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or("incomplete trusted REPL training configuration")?;
        let config = NativeLearningConfig::Collect(NativeLibraryConfig {
            scope: LibraryScope {
                tenant: values[2].clone(),
                project: values[3].clone(),
            },
            database_url: values[4].clone(),
            serve_examples: false,
        });
        config.validate()?;
        let actor = TrustedTrainingActor::authenticated(
            config.scope()?,
            values[0].clone(),
            values[1].clone(),
        )?;
        let mut staged_host = host.clone();
        staged_host.oss.workflow_learning = config.learning();
        let mut staged_execute = session.execute.clone();
        staged_execute.tenant_scope = values[2].clone();
        staged_execute.principal_subject = values[0].clone();
        let logical = plasm_agent::session_identity::LogicalSessionId::new_v4();
        let training = Self {
            config,
            actor,
            logical,
            session_id: logical.to_string(),
            turns: vec!["Local Python authoring session".into()],
        };
        Box::pin(training.config.register(
            &staged_host,
            &staged_execute,
            &training.logical.to_string(),
            &training.session_id,
            &training.turns,
        ))
        .await?;
        staged_host
            .workflow_learning
            .repository()
            .await?
            .bind_training_controller(&training.actor, logical)
            .await?;
        host.oss.workflow_learning = staged_host.oss.workflow_learning;
        session.execute = staged_execute;
        println!("Training session {} is ordinary. Use :training command FILE to request prospective capture.", logical);
        Ok(Some(training))
    }

    async fn publish(
        &self,
        host: &PlasmHostState,
        session: &ProgramSession,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Box::pin(self.config.publish_exposure(
            host,
            &session.execute,
            &self.logical.to_string(),
            &self.session_id,
            &self.turns,
        ))
        .await?;
        Ok(())
    }

    async fn command(
        &self,
        host: &PlasmHostState,
        session: &ProgramSession,
        input: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use plasm_agent::workflow_learning::training::{
            TrainingCommand, TrainingCommandEnvelope, TrainingState,
        };
        let repo = host.workflow_learning.repository().await?;
        repo.check_training_controller(&self.actor, self.logical)
            .await?;
        let (action, argument) = input.split_once(' ').unwrap_or((input, ""));
        let output = match action {
            "status" => serde_json::to_value(repo.training_status(self.actor.scope(), self.logical).await?)?,
            "runs" => self.config.completed_runs(host, &self.logical.to_string(), "Local Python authoring session").await?,
            "candidates" => serde_json::to_value(repo.list_training_candidates(self.actor.scope(), self.logical, None, 32).await?)?,
            "candidate" => {
                let (review, hash) = argument.trim().split_once('.').ok_or("candidate requires REVIEW_UUID.HASH")?;
                let reference = plasm_agent::workflow_learning::repository::TrainingCandidateRef { review_ref: plasm_agent::session_identity::LogicalSessionId::parse(review)?.as_uuid(), candidate_hash: hash.into() };
                if repo.training_candidate_session(self.actor.scope(), &reference).await? != self.logical { return Err("candidate does not belong to this session".into()); }
                serde_json::to_value(repo.read_training_candidate(self.actor.scope(), &reference).await?)?
            }
            "command" => {
                let command: TrainingCommandEnvelope = serde_json::from_str(&std::fs::read_to_string(argument.trim())?)?;
                if !repl_training_command_allowed(&command.command) {
                    return Err("REPL commands request consent or publication; human resolution uses the authenticated approval page".into());
                }
                let status = Box::pin(repo.training_command(&self.actor, self.logical, &command)).await?;
                let request_ref = match &status.training_state {
                    TrainingState::AwaitingAuthorization { request_ref, .. } => Some(*request_ref),
                    _ if matches!(command.command, TrainingCommand::RequestCandidateDecision { .. } | TrainingCommand::RequestRelease { .. } | TrainingCommand::RequestRunRelease { .. }) => Some(command.idempotency_key),
                    _ => None,
                };
                if let Some(reference) = request_ref {
                    if let Ok(base) = std::env::var("PLASM_TRAINING_APPROVAL_BASE_URL") {
                        println!("Authenticated human review: {}/training/requests/{reference}", base.trim_end_matches('/'));
                    } else { println!("Pending request {reference}. Configure PLASM_TRAINING_APPROVAL_BASE_URL to display the authenticated review link."); }
                }
                serde_json::to_value(status)?
            }
            "review" => {
                let body: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(argument.trim())?)?;
                let expected_revision = body["expected_revision"].as_u64().ok_or("review requires expected_revision")?;
                let idempotency_key = plasm_agent::session_identity::LogicalSessionId::parse(body["idempotency_key"].as_str().ok_or("review requires idempotency_key")?)?.as_uuid();
                let subtasks = serde_json::from_value(body["subtasks"].clone())?;
                let review = Box::pin(self.config.review(host, &session.execute, &self.logical.to_string(), &self.session_id, plasm_agent::workflow_learning::native::NativeReview { subtasks, actor: self.actor.clone(), expected_revision, idempotency_key })).await?;
                host.workflow_learning.start_worker();
                review
            }
            _ => return Err("Use :training status | runs | candidates | command FILE | review FILE | candidate REVIEW.HASH".into()),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        Ok(())
    }
}

fn repl_training_command_allowed(
    command: &plasm_agent::workflow_learning::training::TrainingCommand,
) -> bool {
    use plasm_agent::workflow_learning::training::TrainingCommand;
    matches!(
        command,
        TrainingCommand::Request
            | TrainingCommand::Stop
            | TrainingCommand::Resume
            | TrainingCommand::RequestCandidateDecision { .. }
            | TrainingCommand::RequestRelease { .. }
            | TrainingCommand::RequestRunRelease { .. }
    )
}

#[cfg(test)]
mod training_tests {
    use super::*;
    use plasm_agent::workflow_learning::training::TrainingCommand;
    #[test]
    fn repl_training_commands_cannot_supply_human_decisions() {
        assert!(repl_training_command_allowed(&TrainingCommand::Request));
        assert!(repl_training_command_allowed(&TrainingCommand::Stop));
        let reference = plasm_agent::session_identity::LogicalSessionId::new_v4().as_uuid();
        assert!(!repl_training_command_allowed(
            &TrainingCommand::ResolveRequest {
                request_ref: reference,
                request_revision: 0,
                approve: true
            }
        ));
        assert!(!repl_training_command_allowed(
            &TrainingCommand::ResolveRelease {
                request_ref: reference,
                request_revision: 0,
                approve: true
            }
        ));
    }
}
