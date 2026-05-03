//! `plasm-mcp` entry wiring (HTTP + MCP) — OSS data plane: discovery, execute, Streamable HTTP MCP.
//! Tenant MCP policy uses `project_mcp_*` when a config DB URL is set; HTTP **`/v1/traces*`** resolves
//! tenant scope from incoming JWT / API keys ([`plasm_agent_core::incoming_auth`], same env as hosted).
//! The monorepo hosted stack composes `plasm-saas` / `plasm-mcp-app` for product control-plane routes.

use clap::{Arg, ArgAction, Command};
use plasm_agent_core::error::AgentError;
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::{PromptPipelineConfig, PromptRenderMode};
use plasm_plugin_host::PluginManager;
use plasm_runtime::{
    AuthResolver, ExecutionConfig, ExecutionEngine, ExecutionMode, SecretProvider,
};

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM signal handler");
        tokio::select! {
            res = tokio::signal::ctrl_c() => {
                let _ = res;
            }
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn run_mcp_main() -> Result<(), Box<dyn std::error::Error>> {
    plasm_agent_core::init_agent_runtime()?;

    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();

    // Keep this in sync with Helm `deploy/charts/plasm-mcp/values.yaml` default `args` for the
    // *hosted* image. OSS `plasm-mcp` does not run `--migrate-mcp-config-db` (SaaS / ops tooling).
    // Unknown flags here make clap drop earlier flags (e.g. `--plugin-dir`) even with `ignore_errors`.
    let pre_cmd = Command::new("plasm-mcp")
        .arg(
            Arg::new("schema")
                .long("schema")
                .short('s')
                .help("Path to CGS schema file"),
        )
        .arg(
            Arg::new("plugin_dir")
                .long("plugin-dir")
                .value_name("DIR")
                .help(
                    "Load catalogs from self-describing plugin cdylibs in this directory (ABI v4)",
                ),
        )
        .arg(
            Arg::new("symbol_tuning")
                .long("symbol-tuning")
                .value_name("MODE")
                .num_args(1)
                .required(false),
        )
        .arg(Arg::new("http").long("http").action(ArgAction::SetTrue))
        .arg(
            Arg::new("port")
                .long("port")
                .value_name("PORT")
                .num_args(1)
                .required(false),
        )
        .arg(Arg::new("mcp").long("mcp").action(ArgAction::SetTrue))
        .arg(
            Arg::new("mcp_port")
                .long("mcp-port")
                .value_name("PORT")
                .num_args(1)
                .required(false),
        )
        .arg(
            Arg::new("migrate_mcp_config_db")
                .long("migrate-mcp-config-db")
                .action(ArgAction::SetTrue)
                .help(
                    "Run embedded sqlx migrations for tenant MCP tables (`project_mcp_*`), then exit. \
Uses PLASM_MCP_CONFIG_DATABASE_URL, else PLASM_AUTH_STORAGE_URL, else DATABASE_URL.",
                ),
        )
        .ignore_errors(true);

    let pre_matches = pre_cmd.get_matches_from(&argv);

    if pre_matches.get_flag("migrate_mcp_config_db") {
        let Some(db_url) = plasm_agent_core::mcp_config_repository::mcp_config_database_url()
        else {
            eprintln!(
                "plasm-mcp: --migrate-mcp-config-db requires PLASM_MCP_CONFIG_DATABASE_URL, PLASM_AUTH_STORAGE_URL, or DATABASE_URL"
            );
            std::process::exit(1);
        };
        plasm_agent_core::mcp_config_repository::McpConfigRepository::connect_and_migrate(&db_url)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> {
                format!("MCP config database migrate failed: {e}").into()
            })?;
        tracing::info!("MCP configuration sqlx migrations applied successfully");
        return Ok(());
    }

    let plugin_dir = pre_matches.get_one::<String>("plugin_dir");
    let server_mode = pre_matches.get_flag("http") || pre_matches.get_flag("mcp");

    let (schema_path, cgs, preloaded_opt) = match pre_matches.get_one::<String>("schema") {
        Some(path) => {
            if plugin_dir.is_some() {
                eprintln!("plasm-mcp: do not combine --schema with --plugin-dir");
                std::process::exit(1);
            }
            let cgs = plasm_core::loader::load_schema(std::path::Path::new(path))
                .map_err(AgentError::Schema)?;
            (path.clone(), cgs, None)
        }
        None => {
            if server_mode {
                if let Some(pd) = plugin_dir {
                    let reg = plasm_agent_core::plugin_catalog::load_registry_from_plugin_dir(
                        std::path::Path::new(pd),
                    )
                    .map_err(AgentError::Schema)?;
                    let arc_cgs = reg.first_cgs().ok_or_else(|| {
                        AgentError::Schema("plugin-dir catalog has no entries".to_string())
                    })?;
                    let cgs = (*arc_cgs).clone();
                    (pd.clone(), cgs, Some(reg))
                } else {
                    eprintln!("Usage: plasm-mcp --schema <path> [--http] [--mcp] …");
                    eprintln!(
                        "   or: plasm-mcp --plugin-dir <dir> --http and/or --mcp (multi-entry plugin catalogs)"
                    );
                    std::process::exit(1);
                }
            } else {
                eprintln!("Usage: plasm-mcp --schema <path> [--http] [--mcp] …");
                std::process::exit(1);
            }
        }
    };

    if let Some(reg) = &preloaded_opt {
        plasm_agent_core::plugin_catalog::validate_registry_templates(reg)
            .map_err(AgentError::Schema)?;
    } else {
        plasm_compile::validate_cgs_capability_templates(&cgs)
            .map_err(|e| AgentError::Schema(e.to_string()))?;
    }

    let app = plasm_agent_core::cli_builder::build_app(
        &cgs,
        plasm_agent_core::AgentCliSurface::McpServer,
    );
    let matches = app.get_matches_from(&argv);

    if matches.get_one::<String>("schema").is_some()
        && matches.get_one::<String>("plugin_dir").is_some()
    {
        eprintln!("plasm-mcp: do not combine --schema with --plugin-dir");
        std::process::exit(1);
    }

    let backend_raw = matches
        .get_one::<String>("backend")
        .map(|s| s.as_str())
        .unwrap_or("http://localhost:1080");
    let backend = plasm_agent_core::backend_normalize::normalize_live_backend_url(
        schema_path.as_str(),
        backend_raw,
    );

    let mode = match matches
        .get_one::<String>("mode")
        .map(|s| s.as_str())
        .unwrap_or("live")
    {
        "replay" => ExecutionMode::Replay,
        "hybrid" => ExecutionMode::Hybrid,
        _ => ExecutionMode::Live,
    };

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

    let auth_resolver = cgs.auth.clone().map(AuthResolver::from_env);
    let engine = ExecutionEngine::new_with_auth(config, auth_resolver)?;

    let registry = std::sync::Arc::new(if let Some(reg) = preloaded_opt {
        reg
    } else if let Some(pd) = matches.get_one::<String>("plugin_dir") {
        plasm_agent_core::plugin_catalog::load_registry_from_plugin_dir(std::path::Path::new(pd))
            .map_err(AgentError::Schema)?
    } else {
        InMemoryCgsRegistry::from_pairs(vec![(
            "default".into(),
            "default".into(),
            vec![],
            std::sync::Arc::new(cgs.clone()),
        )])
    });

    let catalog_bootstrap = if matches.get_one::<String>("plugin_dir").is_some() {
        CatalogBootstrap::PluginDir {
            path: std::path::PathBuf::from(
                matches
                    .get_one::<String>("plugin_dir")
                    .expect("plugin-dir checked"),
            ),
        }
    } else {
        CatalogBootstrap::Fixed
    };

    let use_http = matches.get_flag("http");
    let use_mcp = matches.get_flag("mcp");
    let port = *matches.get_one::<u16>("port").unwrap_or(&3000);

    if !use_http && !use_mcp {
        eprintln!("plasm-mcp: pass --http and/or --mcp");
        std::process::exit(1);
    }

    let plugin_manager = match matches.get_one::<String>("compile_plugin") {
        Some(path) => {
            let pm = PluginManager::load(std::path::Path::new(path))
                .map_err(|e| std::io::Error::other(format!("--compile-plugin {path}: {e}")))?;
            Some(std::sync::Arc::new(pm))
        }
        None => None,
    };
    let run_artifacts = plasm_agent_core::run_artifacts::init_from_env_with_policy(
        plasm_agent_core::run_artifacts::RunArtifactInitPolicy::OssFilesystemDefaults,
    )
    .map_err(|e| std::io::Error::other(format!("run artifacts: {e}")))?;
    let session_graph_persistence = plasm_agent_core::session_graph_persistence::init_from_env()
        .map_err(|e| std::io::Error::other(format!("session graph persistence: {e}")))?;

    let incoming_cfg = plasm_agent_core::incoming_auth::IncomingAuthConfig::from_env();
    if let Err(e) = incoming_cfg.validate_startup() {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let incoming_verifier = std::sync::Arc::new(
        plasm_agent_core::incoming_auth::IncomingAuthVerifier::new(incoming_cfg.clone())
            .map_err(std::io::Error::other)?,
    );
    plasm_agent_core::incoming_auth::log_incoming_auth_startup(&incoming_cfg, &incoming_verifier);

    let mut app_state = plasm_agent_core::http::build_plasm_host_state(
        plasm_agent_core::http::PlasmHostBootstrap {
            engine,
            mode,
            registry,
            catalog_bootstrap,
            plugin_manager,
            incoming_auth: Some(incoming_verifier),
            run_artifacts,
            session_graph_persistence,
            oss_local_filesystem_defaults: true,
        },
    );

    let outbound_enabled = std::env::var("PLASM_OUTBOUND_OAUTH")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("PLASM_AUTH_STORAGE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_some()
        || std::env::var("DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_some();

    if outbound_enabled {
        match plasm_agent_core::auth_framework_host::init_standalone_auth_storage().await {
            Ok(storage) => {
                let catalog = std::sync::Arc::new(
                    plasm_agent_core::oauth_link_catalog::OauthLinkCatalog::from_env(),
                );
                let outbound = std::sync::Arc::new(
                    plasm_agent_core::outbound_secret_provider::AgentOutboundSecretProvider::new(
                        storage.clone(),
                        catalog.clone(),
                    ),
                );
                app_state.oss.auth_storage = Some(storage);
                app_state.oss.oauth_link_catalog = Some(catalog);
                app_state.oss.outbound_secret_provider =
                    Some(outbound as std::sync::Arc<dyn SecretProvider>);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "outbound OAuth: auth storage init failed; set PLASM_AUTH_STORAGE_URL or DATABASE_URL, or PLASM_OUTBOUND_OAUTH=1 with a valid storage configuration"
                );
            }
        }
    }

    maybe_attach_oss_mcp_policy_store(&mut app_state).await;

    let mcp_port = match matches.get_one::<u16>("mcp_port").copied() {
        Some(p) => p,
        None if use_http && use_mcp => port.saturating_add(1),
        None => port,
    };
    if use_http && use_mcp && mcp_port == port {
        eprintln!("--http and --mcp cannot share the same port; set --mcp-port explicitly.");
        std::process::exit(1);
    }

    if use_http && use_mcp {
        let st = std::sync::Arc::new(app_state);
        let st_http = (*st).clone();
        let st_mcp = std::sync::Arc::clone(&st);
        tokio::select! {
            _ = shutdown_signal() => {
                eprintln!("plasm-mcp: shutting down");
            }
            res = async {
                tokio::try_join!(
                    async {
                        plasm_agent_core::http::serve_http_listener(st_http, port)
                            .await
                            .map_err(|e| {
                                std::io::Error::other(format!("plasm-mcp HTTP server: {e}"))
                            })
                    },
                    async {
                        plasm_agent_core::mcp_server::run_mcp_server("0.0.0.0", mcp_port, st_mcp)
                            .await
                            .map_err(|e| {
                                std::io::Error::other(format!("plasm-mcp MCP server: {e}"))
                            })
                    },
                )
            } => {
                res?;
            }
        }
        return Ok(());
    }
    if use_http {
        tokio::select! {
            _ = shutdown_signal() => {
                eprintln!("plasm-mcp: shutting down");
            }
            r = plasm_agent_core::http::serve_http_listener(app_state, port) => {
                r?;
            }
        }
        return Ok(());
    }
    tokio::select! {
        _ = shutdown_signal() => {
            eprintln!("plasm-mcp: shutting down");
        }
        r = plasm_agent_core::mcp_server::run_mcp_server("0.0.0.0", mcp_port, std::sync::Arc::new(app_state)) => {
            r.map_err(|e| std::io::Error::other(format!("plasm-mcp MCP server: {e}")))?;
        }
    }
    Ok(())
}

/// When `DATABASE_URL` / `PLASM_MCP_CONFIG_DATABASE_URL` / `PLASM_AUTH_STORAGE_URL` resolves, enable
/// `project_mcp_*` persistence and MCP transport API keys (same handlers as hosted `/internal/*`).
async fn maybe_attach_oss_mcp_policy_store(
    app_state: &mut plasm_agent_core::server_state::PlasmHostState,
) {
    let Some(db_url) = plasm_agent_core::mcp_config_repository::mcp_config_database_url() else {
        return;
    };
    let repo =
        match plasm_agent_core::mcp_config_repository::McpConfigRepository::connect_and_migrate(
            &db_url,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "OSS plasm-mcp: project_mcp_* connect/migrate failed; tenant MCP policy disabled"
                );
                return;
            }
        };

    let mcp_keys = if let Some(s) = app_state.oss.auth_storage.clone() {
        std::sync::Arc::new(plasm_agent_core::mcp_api_key_registry::McpApiKeyRegistry::new(s))
    } else {
        match plasm_agent_core::auth_framework_host::init_standalone_auth_storage().await {
            Ok(s) => {
                app_state.oss.auth_storage = Some(s.clone());
                std::sync::Arc::new(
                    plasm_agent_core::mcp_api_key_registry::McpApiKeyRegistry::new(s),
                )
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "OSS plasm-mcp: durable auth KV unavailable; MCP API keys are in-memory only until restart (set DATABASE_URL / PLASM_AUTH_STORAGE_URL for durable keys)"
                );
                plasm_agent_core::auth_framework_host::mcp_api_key_registry_memory_only()
            }
        }
    };
    app_state.saas = Some(plasm_agent_core::server_state::PlasmSaaSHostExtension {
        auth_framework: None,
        mcp_config_repository: Some(std::sync::Arc::new(repo)),
        mcp_transport_auth: Some(mcp_keys),
        tenant_binding: None,
    });
    tracing::info!(
        "OSS plasm-mcp: tenant MCP policy enabled (project_mcp_* + API keys); control-plane routes on HTTP require X-Plasm-Control-Plane-Secret"
    );
}
