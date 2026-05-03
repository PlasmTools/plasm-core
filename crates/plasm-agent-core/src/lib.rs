//! `plasm-agent` library: HTTP/MCP SaaS core and schema CLI — built as **`plasm-mcp`** and **`plasm-cgs`**.
//! Interactive REPL (`plasm-repl`) lives in **`plasm-repl`** (depends on `plasm-eval` / BAML). Integration tests live under `tests/*.rs`.

pub mod appliance_mcp_defaults;
pub mod auth_framework_host;
mod auth_framework_postgres_schema;
pub mod backend_normalize;
pub mod bootstrap_secrets;
pub mod catalog_runtime;
pub mod cli_builder;
pub mod control_plane_http;
pub mod dispatch;
pub mod dotenv_safe;
pub mod error;
pub mod execute_path_ids;
pub mod execute_session;
mod execute_staging;
pub mod expr_display;
pub mod http;
pub mod http_discovery;
pub mod http_execute;
pub mod http_incoming_context;
pub mod http_mcp_config;
pub mod http_oauth_link;
pub mod http_outbound_secrets;
pub mod http_problem_util;
mod http_traces;
pub mod incoming_auth;
pub mod input_field_cli;
pub mod invoke_args;
pub mod local_trace_archive;
pub mod mcp_api_key_registry;
pub mod mcp_config_repository;
pub mod mcp_plasm_meta;
pub mod mcp_policy;
mod mcp_run_markdown;
pub mod mcp_runtime_config;
pub mod mcp_server;
mod mcp_stream_auth;
pub mod mcp_transport_auth;
pub mod metrics;
pub mod oauth_link_catalog;
pub mod oauth_link_session;
mod oauth_provider_model;
pub mod oauth_provider_pull;
mod oauth_runtime_source;
pub mod oss_local_state;
pub mod outbound_secret_provider;
pub mod output;
pub mod plasm_dag;
/// Serializable effect [`Plan`](plasm_plan::Plan) contract and DAG validation (Plasm programs, archived plans).
pub mod plasm_plan;
pub mod plasm_plan_run;
pub mod plugin_catalog;
pub mod query_args;
pub mod run_artifacts;
pub mod server_state;
pub mod session_graph_persistence;
pub mod session_identity;
pub mod spans;
pub mod subcommand_util;
mod telemetry;
pub mod tenant_binding;
mod tool_model;
pub mod trace_hub;
pub(crate) mod trace_hub_metrics;
pub mod trace_sink_emit;
mod web_connected_account_notify;

use clap::{Arg, Command};
use plasm_core::PromptPipelineConfig;
use plasm_runtime::{AuthResolver, ExecutionConfig, ExecutionEngine, ExecutionMode, GraphCache};

pub use crate::cli_builder::AgentCliSurface;
use crate::error::AgentError;
use crate::output::OutputFormat;

/// Dotenv + telemetry init shared with `plasm-repl` and other front binaries.
pub fn init_agent_runtime() -> Result<(), Box<dyn std::error::Error>> {
    crate::dotenv_safe::load_from_cwd_parents();
    crate::telemetry::init().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

/// Schema-driven one-shot CLI (`plasm-cgs` binary).
pub async fn run_cgs_main() -> Result<(), Box<dyn std::error::Error>> {
    init_agent_runtime()?;

    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();

    let pre_cmd = Command::new("plasm-cgs")
        .disable_help_flag(true)
        .arg(
            Arg::new("schema")
                .long("schema")
                .short('s')
                .help("Path to CGS schema file"),
        )
        .ignore_errors(true);

    let pre_matches = pre_cmd.get_matches_from(&argv);

    let Some(schema_path) = pre_matches.get_one::<String>("schema").cloned() else {
        eprintln!("plasm-cgs: --schema <path> is required");
        std::process::exit(1);
    };

    let cgs = plasm_core::loader::load_schema(std::path::Path::new(&schema_path))
        .map_err(AgentError::Schema)?;

    plasm_compile::validate_cgs_capability_templates(&cgs)
        .map_err(|e| AgentError::Schema(e.to_string()))?;

    let app = cli_builder::build_app(&cgs, AgentCliSurface::CgsClient);
    let matches = app.get_matches_from(&argv);

    let backend_raw = matches
        .get_one::<String>("backend")
        .map(|s| s.as_str())
        .unwrap_or("http://localhost:1080");
    let backend = backend_normalize::normalize_live_backend_url(schema_path.as_str(), backend_raw);

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
    let prompt_pipeline = PromptPipelineConfig::for_cli_focus(prompt_focus.as_deref());

    let config = ExecutionConfig {
        base_url: Some(backend.to_string()),
        prompt_pipeline,
        ..ExecutionConfig::default()
    };

    let auth_resolver = cgs.auth.clone().map(AuthResolver::from_env);
    let engine = ExecutionEngine::new_with_auth(config, auth_resolver)?;

    if matches.subcommand().is_none() {
        eprintln!("plasm-cgs: provide an entity subcommand (see --help).");
        std::process::exit(1);
    }

    let mut cache = GraphCache::new();
    dispatch::dispatch(&matches, &cgs, &engine, &mut cache, mode, output_format).await?;

    Ok(())
}
