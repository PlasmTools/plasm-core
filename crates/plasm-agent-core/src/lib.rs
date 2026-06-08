//! `plasm-agent-core` library: HTTP/MCP host engine — built as **`plasm-mcp`** and **`plasm`** (remote HTTP terminal) via the workspace **`plasm`** crate.
//! Interactive REPL (`plasm-repl`) lives in **`plasm-repl`** (depends on `plasm-eval` / BAML). Integration tests live under `tests/*.rs`.

// Large MCP tool async stacks + `#[async_trait]` boxing can exceed the default trait solver recursion
// budget when proving nested HTTP/hyper futures are `Send` (rustc 1.87+).
#![recursion_limit = "512"]

pub mod appliance_mcp_defaults;
pub mod appliance_services;
pub mod auth_framework_host;
mod auth_framework_postgres_schema;
pub mod backend_normalize;
pub mod binding_slots;
pub mod binding_store;
pub mod bootstrap_secrets;
pub(crate) mod catalog_ownership;
pub(crate) mod catalog_pin;
pub mod catalog_runtime;
pub mod cli_builder;
pub mod control_plane_http;
mod discovery_embedding_chunks;
#[cfg(feature = "local-embeddings")]
pub mod discovery_embedding_reconcile;
pub mod discovery_embedding_repository;
mod discovery_human_format;
pub mod dispatch;
pub mod dotenv_safe;
pub mod error;
pub mod execute_path_ids;
pub mod execute_pipeline;
pub mod execute_session;
pub mod expr_display;
mod graph_cache_metrics;
mod graph_page_spill_host;
mod graph_rehydrate;
pub mod http;
pub mod http_backend;
pub mod http_discovery;
pub mod http_execute;
pub mod http_incoming_context;
pub mod http_mcp_bindings;
pub mod http_mcp_config;
pub mod http_oauth_link;
pub mod http_outbound_secrets;
pub mod http_problem_util;
mod http_traces;
pub mod incoming_auth;
pub mod incoming_auth_device;
pub mod input_field_cli;
pub mod invoke_args;
pub mod listen_endpoint;
pub mod local_trace_archive;
pub mod mcp_api_key_registry;
pub mod mcp_config_admin;
pub mod mcp_config_readiness;
pub mod mcp_config_repository;
pub mod mcp_host_bootstrap;
pub mod mcp_plasm_meta;
pub mod mcp_policy;
mod mcp_run_markdown;
pub mod mcp_runtime_config;
pub mod mcp_server;
mod mcp_stream_auth;
pub mod mcp_transport_auth;
pub mod metrics;
pub mod oauth_binding_kv;
pub mod oauth_link_catalog;
pub mod oauth_link_session;
pub mod oauth_provider_model;
pub mod oauth_provider_pull;
pub mod oauth_provider_repository;
pub mod oauth_runtime_source;
pub mod operation;
mod operation_progress;
#[cfg(test)]
mod operation_progress_coalesce_integration;
pub mod oss_local_state;
pub mod outbound_secret_provider;
pub mod output;
mod plan_dry_display;
pub mod plan_read_bounds;
pub mod plasm_dag;
/// Serializable effect [`Plan`](plasm_plan::Plan) contract and DAG validation (Plasm programs, archived plans).
pub mod plasm_plan;
pub mod plasm_plan_run;
pub mod plugin_catalog;
mod program_binding;
pub mod query_args;
pub(crate) mod resolved_plan_http;
pub(crate) mod row_predicate_lower;
pub mod run_artifacts;
pub mod schema_overlay_session;
pub mod server_state;
pub mod session_bindings;
pub mod session_graph_persistence;
pub use graph_page_spill_host::graph_page_spill_for_execute;
pub mod session_identity;
pub mod spans;
mod stream_consume;
pub mod subcommand_util;
mod telemetry;
pub mod tenant_binding;
pub mod terminal;
mod terminal_cli;
mod terminal_mirror;
mod terminal_session;
mod terminal_state;
pub use terminal::DEFAULT_PLASM_HTTP_ORIGIN;
mod tool_model;
pub mod trace_hub;
pub(crate) mod trace_hub_metrics;
pub mod trace_sink_emit;
pub mod typed_discovery_host;
mod web_connected_account_notify;

pub use crate::cli_builder::AgentCliSurface;

/// Dotenv + telemetry init shared with `plasm-repl` and other front binaries.
pub fn init_agent_runtime() -> Result<(), Box<dyn std::error::Error>> {
    crate::dotenv_safe::load_from_cwd_parents();
    crate::telemetry::init().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

/// Like [`init_agent_runtime`], but installs the `tracing` **fmt** layer with `make_writer` (e.g. alternate-screen TUIs).
pub fn init_agent_runtime_with_fmt_writer<W>(
    make_writer: W,
) -> Result<(), Box<dyn std::error::Error>>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + Clone + 'static,
{
    crate::dotenv_safe::load_from_cwd_parents();
    crate::telemetry::init_with_fmt_make_writer(make_writer)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

/// Appliance TUI: fmt to `make_writer` (diag file or sink) plus typed log capture for the Logs tab.
pub fn init_agent_runtime_with_appliance_logs<W>(
    make_writer: W,
    tui_capture: Option<plasm_otel::TuiLogCallback>,
) -> Result<(), Box<dyn std::error::Error>>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + Clone + 'static,
{
    crate::dotenv_safe::load_from_cwd_parents();
    crate::telemetry::init_with_fmt_make_writer_and_tui(make_writer, tui_capture)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

/// Remote HTTP terminal (`plasm` binary from the workspace `plasm` crate). Local schema-driven CGS CLIs use `plasm-repl` / tests only.
pub async fn run_cgs_main() -> Result<(), Box<dyn std::error::Error>> {
    crate::terminal::run_terminal()
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })
}
