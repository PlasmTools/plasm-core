//! Interactive REPL binary: path expressions, optional `:llm` mode via BAML (`plasm-eval`).
//!
//! Split from `plasm` so HTTP/MCP builds and CI do not compile `plasm-eval` / generated `baml_client`.

use clap::{Arg, Command};
use plasm_agent::error::AgentError;
use plasm_agent::output::OutputFormat;
use plasm_agent::{
    backend_normalize, catalog_data, cli_builder, init_agent_runtime, AgentCliSurface,
};
use plasm_core::{PromptPipelineConfig, PromptRenderMode};
use plasm_runtime::{AuthResolver, ExecutionConfig, ExecutionEngine, ExecutionMode};

mod repl;

/// Entry point for the `plasm-repl` binary (`:commands`, optional OpenRouter + BAML).
pub async fn run_repl_main() -> Result<(), Box<dyn std::error::Error>> {
    init_agent_runtime()?;

    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();

    let pre_cmd = Command::new("plasm-repl")
        .disable_help_flag(true)
        .arg(
            Arg::new("schema")
                .long("schema")
                .short('s')
                .help("Path to CGS schema file"),
        )
        .arg(
            Arg::new("catalog_dir")
                .long("catalog-dir")
                .value_name("DIR")
                .help("Compiled catalog artifact directory (JSON IL + manifests)"),
        )
        .ignore_errors(true);

    let pre_matches = pre_cmd.get_matches_from(&argv);

    let catalog_dir = pre_matches.get_one::<String>("catalog_dir");

    let (schema_path, cgs, templates_prevalidated) = match pre_matches.get_one::<String>("schema") {
        Some(path) => {
            if catalog_dir.is_some() {
                eprintln!("plasm-repl: do not combine --schema with --catalog-dir");
                std::process::exit(1);
            }
            let cgs = plasm_core::loader::load_schema(std::path::Path::new(path))
                .map_err(AgentError::Schema)?;
            (path.clone(), cgs, false)
        }
        None => {
            if let Some(pd) = catalog_dir {
                let reg = catalog_data::load_registry_from_catalog_dir(std::path::Path::new(pd))
                    .map_err(AgentError::Schema)?;
                let arc_cgs = reg.first_cgs().ok_or_else(|| {
                    AgentError::Schema("catalog-dir catalog has no entries".into())
                })?;
                let cgs = (*arc_cgs).clone();
                (pd.clone(), cgs, true)
            } else {
                eprintln!("plasm-repl: pass --schema <path> or --catalog-dir <dir>");
                std::process::exit(1);
            }
        }
    };

    if !templates_prevalidated {
        plasm_compile::validate_cgs_capability_templates(&cgs)
            .map_err(|e| AgentError::Schema(e.to_string()))?;
    }

    let app = cli_builder::build_app(&cgs, AgentCliSurface::Repl);
    let matches = app.get_matches_from(&argv);

    let backend_raw = matches
        .get_one::<String>("backend")
        .map(|s| s.as_str())
        .unwrap_or("http://localhost:1080");
    let backend = backend_normalize::normalize_live_backend_url(schema_path.as_str(), backend_raw);
    plasm_agent::http_backend::ReplHttpOverride::from_cli_normalized(backend.as_ref())
        .map_err(|e| AgentError::Argument(e.to_string()))?;

    let mode = match matches
        .get_one::<String>("mode")
        .map(|s| s.as_str())
        .unwrap_or("live")
    {
        "replay" => ExecutionMode::Replay,
        "hybrid" => ExecutionMode::Hybrid,
        _ => ExecutionMode::Live,
    };

    let output_format = OutputFormat::parse(
        matches
            .get_one::<String>("output")
            .map(|s| s.as_str())
            .unwrap_or("json"),
    );

    let prompt_focus = matches.get_one::<String>("focus").cloned();
    let render_mode = matches
        .get_one::<String>("symbol_tuning")
        .map(|s| PromptRenderMode::parse_user_facing_or_default(s))
        .unwrap_or_default();
    let prompt_pipeline =
        PromptPipelineConfig::for_cli_focus(prompt_focus.as_deref()).with_render_mode(render_mode);

    let config = ExecutionConfig {
        base_url: Some(backend.to_string()),
        prompt_pipeline,
        ..ExecutionConfig::default()
    };

    let auth_scheme = cgs.auth.clone();
    let auth_resolver = auth_scheme.clone().map(AuthResolver::from_env);
    let engine = ExecutionEngine::new_with_auth(config, auth_resolver)?;
    let base_cgs = std::sync::Arc::new(cgs);
    let cgs = plasm_agent::schema_overlay_session::resolve_schema_overlay_for_local(
        &engine,
        mode,
        base_cgs,
        backend.as_ref(),
        schema_path.as_str(),
    )
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    plasm_agent::schema_overlay_session::eprint_schema_overlay_status(cgs.as_ref());

    repl::run_repl(cgs.as_ref(), &engine, mode, output_format, prompt_focus).await?;

    Ok(())
}
