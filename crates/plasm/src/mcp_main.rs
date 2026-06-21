//! `plasm-mcp` entry wiring (HTTP + MCP) — OSS data plane: discovery, execute, Streamable HTTP MCP.
//! Tenant MCP policy uses `project_mcp_*` when a config DB URL is set; HTTP **`/v1/traces*`** resolves
//! tenant scope from incoming JWT / API keys ([`plasm_agent_core::incoming_auth`], same env as hosted).
//! The monorepo hosted stack composes `plasm-saas` / `plasm-mcp-app` for product control-plane routes.

use plasm_agent_core::mcp_host_bootstrap;

async fn shutdown_embedded_pg(slot: &mut Option<crate::embedded_postgres::EmbeddedPostgresGuard>) {
    if let Some(g) = slot.take() {
        if let Err(e) = g.shutdown().await {
            tracing::warn!(
                error = %e,
                "embedded postgres: graceful shutdown failed"
            );
        }
    }
}

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
    // Unknown flags here make clap drop earlier flags (e.g. `--catalog-dir`) even with `ignore_errors`.
    let pre_matches = mcp_host_bootstrap::preparse_mcp_command().get_matches_from(&argv);

    if pre_matches.get_flag("migrate_mcp_config_db") {
        let mut embedded_pg =
            crate::embedded_postgres::EmbeddedPostgresGuard::try_start_from_env().await?;
        let Some(db_url) = plasm_agent_core::mcp_config_repository::mcp_config_database_url()
        else {
            shutdown_embedded_pg(&mut embedded_pg).await;
            eprintln!(
                "plasm-mcp: --migrate-mcp-config-db requires PLASM_MCP_CONFIG_DATABASE_URL, PLASM_AUTH_STORAGE_URL, or DATABASE_URL"
            );
            std::process::exit(1);
        };
        let migrate_result =
            plasm_agent_core::mcp_config_repository::McpConfigRepository::connect_and_migrate(
                &db_url,
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error> {
                format!("MCP config database migrate failed: {e}").into()
            });
        shutdown_embedded_pg(&mut embedded_pg).await;
        migrate_result?;
        tracing::info!("MCP configuration sqlx migrations applied successfully");
        return Ok(());
    }

    let server_mode = pre_matches.get_flag("http") || pre_matches.get_flag("mcp");

    let catalog_outcome = match mcp_host_bootstrap::load_catalog_for_mcp_server(
        &pre_matches,
        server_mode,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("plasm-mcp: {e}");
            eprintln!("Usage: plasm-mcp --schema <path> [--http] [--mcp] …");
            eprintln!(
                "   or: plasm-mcp --catalog-dir <dir> --http and/or --mcp (multi-entry plugin catalogs)"
            );
            std::process::exit(1);
        }
    };

    if let Err(e) = mcp_host_bootstrap::validate_catalog_templates(&catalog_outcome) {
        eprintln!("plasm-mcp: {e}");
        std::process::exit(1);
    }

    let app = plasm_agent_core::cli_builder::build_app(
        &catalog_outcome.cgs,
        plasm_agent_core::AgentCliSurface::McpServer,
    );
    let matches = app.get_matches_from(&argv);

    if matches.get_one::<String>("schema").is_some()
        && matches.get_one::<String>("catalog_dir").is_some()
    {
        eprintln!("plasm-mcp: do not combine --schema with --catalog-dir");
        std::process::exit(1);
    }

    let use_http = matches.get_flag("http");
    let use_mcp = matches.get_flag("mcp");
    let endpoint =
        plasm_agent_core::listen_endpoint::TcpListenEndpoint::from_clap_matches(&matches)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    if !use_http && !use_mcp {
        eprintln!("plasm-mcp: pass --http and/or --mcp");
        std::process::exit(1);
    }

    let mut embedded_pg =
        crate::embedded_postgres::EmbeddedPostgresGuard::try_start_from_env().await?;

    let host_bootstrap = match mcp_host_bootstrap::bootstrap_plasm_host_state_oss(
        &matches,
        &catalog_outcome,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            shutdown_embedded_pg(&mut embedded_pg).await;
            return Err(e.into());
        }
    };
    let app_state = host_bootstrap.state;

    let mcp_port = match matches.get_one::<u16>("mcp_port").copied() {
        Some(p) => p,
        None => endpoint.port,
    };
    if use_http && use_mcp {
        if matches.get_one::<u16>("mcp_port").is_some() && mcp_port != endpoint.port {
            eprintln!(
                "plasm-mcp: with --http and --mcp, discovery/execute and MCP share --port; omit --mcp-port or set it equal to --port."
            );
            std::process::exit(1);
        }
        let state = app_state;
        let listen = endpoint.clone();
        tokio::select! {
            _ = shutdown_signal() => {
                eprintln!("plasm-mcp: shutting down");
            }
            res = async {
                let listener = listen.bind_tcp_listener().await.map_err(|e| {
                    std::io::Error::other(format!("plasm-mcp bind {}: {e}", listen.display_addr()))
                })?;
                plasm_agent_core::http::serve_discovery_execute_and_mcp_unified(
                    listener,
                    state,
                    plasm_agent_core::http::DiscoveryHttpServeOpts::default(),
                )
                .await
                .map_err(|e| std::io::Error::other(format!("plasm-mcp unified server: {e}")))
            } => {
                res?;
            }
        }
        shutdown_embedded_pg(&mut embedded_pg).await;
        return Ok(());
    }
    if use_http {
        let listen = endpoint.clone();
        tokio::select! {
            _ = shutdown_signal() => {
                eprintln!("plasm-mcp: shutting down");
            }
            r = plasm_agent_core::http::serve_http_listener(app_state, listen) => {
                r.map_err(|e| std::io::Error::other(format!("{e}")))?;
            }
        }
        shutdown_embedded_pg(&mut embedded_pg).await;
        return Ok(());
    }
    let host = endpoint.host.clone();
    let app_state = plasm_agent_core::mcp_transport_store::prepare_host_for_serve(app_state)
        .await
        .map_err(|e| std::io::Error::other(format!("Redis wiring: {e}")))?;
    tokio::select! {
        _ = shutdown_signal() => {
            eprintln!("plasm-mcp: shutting down");
        }
        r = plasm_agent_core::mcp_server::run_mcp_server(&host, mcp_port, std::sync::Arc::new(app_state)) => {
            r.map_err(|e| std::io::Error::other(format!("plasm-mcp MCP server: {e}")))?;
        }
    }
    shutdown_embedded_pg(&mut embedded_pg).await;
    Ok(())
}
