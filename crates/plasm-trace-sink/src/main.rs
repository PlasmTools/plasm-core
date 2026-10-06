//! Binary entry: `plasm-trace-sink`.
//!
//! Startup is structured so every failure path logs (`tracing::error`) and writes a plain line to
//! stderr before exit code 1 — silent exits are unacceptable for operators (`kubectl logs`).

use std::io::Write;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::error::ErrorKind;
use clap::Parser;
use plasm_trace_sink::append_port::AuditSpanStore;
use plasm_trace_sink::config::{Config, WarehouseLocation};
use plasm_trace_sink::http::router;
use plasm_trace_sink::iceberg_writer::IcebergSink;
use plasm_trace_sink::persisted::PersistedTraceSink;
use plasm_trace_sink::state::AppState;
use thiserror::Error;

#[derive(Parser, Debug)]
#[command(name = "plasm-trace-sink")]
struct Args {
    /// Override listen address (else `PLASM_TRACE_SINK_LISTEN` or default).
    #[arg(long)]
    listen: Option<String>,
}

#[derive(Debug, Error)]
enum TraceSinkRunError {
    #[error("command-line parsing failed")]
    Arguments(#[source] clap::Error),
    #[error(transparent)]
    Configuration(#[from] plasm_trace_sink::config::TraceSinkConfigError),
    #[error("could not create {operation} at {path}")]
    CreateDirectory {
        operation: TraceSinkDirectory,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Iceberg sink connection failed")]
    Iceberg(#[source] plasm_trace_sink::storage_error::IcebergWriterError),
    #[error("persisted trace sink connection failed")]
    Persisted(#[source] plasm_trace_sink::storage_error::TraceSinkStorageError),
    #[error("listen address is invalid")]
    ListenAddress(#[source] std::net::AddrParseError),
    #[error("binding the HTTP listener failed")]
    Bind(#[source] std::io::Error),
    #[error("HTTP server terminated with an error")]
    Serve(#[source] std::io::Error),
}

#[derive(Debug, Clone, Copy, Error)]
enum TraceSinkDirectory {
    #[error("data directory")]
    Data,
    #[error("warehouse directory")]
    Warehouse,
}

fn install_panic_hook() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::io::stderr().write_all(format!("plasm-trace-sink panic: {info}\n").as_bytes());
        let _ = std::io::stderr().flush();
        default_panic(info);
    }));
}

fn init_tracing() {
    if let Err(e) = plasm_otel::init("plasm-trace-sink") {
        let _ = writeln!(
            std::io::stderr(),
            "plasm-trace-sink FATAL: tracing / OpenTelemetry init failed: {e:#}"
        );
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }
}

fn log_fatal_and_exit(err: &TraceSinkRunError) -> ! {
    // tracing first (structured), then a single guaranteed line for log collectors / systemd.
    tracing::error!(error = %err, error_debug = ?err, "plasm-trace-sink fatal startup or runtime error");
    let _ = writeln!(
        std::io::stderr(),
        "plasm-trace-sink FATAL: {}",
        render_error_chain(err)
    );
    let _ = std::io::stderr().flush();
    std::process::exit(1);
}

fn render_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut rendered = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        rendered.push_str(": ");
        rendered.push_str(&cause.to_string());
        source = cause.source();
    }
    rendered
}

#[tokio::main]
async fn main() -> ExitCode {
    install_panic_hook();
    init_tracing();

    tracing::info!("plasm-trace-sink process starting");
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log_fatal_and_exit(&e);
        }
    }
}

async fn run() -> Result<(), TraceSinkRunError> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            e.exit()
        }
        Err(e) => return Err(TraceSinkRunError::Arguments(e)),
    };

    Config::ensure_iceberg_not_disabled()?;

    let mut config = Config::from_env();
    if let Some(l) = args.listen {
        config.listen = l;
    }

    tracing::info!(
        data_dir = %config.data_dir.display(),
        warehouse_fs_path = %config.warehouse_fs_path.display(),
        warehouse_s3_url_set = config.warehouse_s3_url.is_some(),
        catalog_url_set = config.catalog_url.is_some(),
        "config loaded"
    );

    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .map_err(|source| TraceSinkRunError::CreateDirectory {
            operation: TraceSinkDirectory::Data,
            path: config.data_dir.clone(),
            source,
        })?;

    let connect = config.iceberg_connect_params()?;

    if let WarehouseLocation::Filesystem(root) = &connect.warehouse {
        tokio::fs::create_dir_all(root).await.map_err(|source| {
            TraceSinkRunError::CreateDirectory {
                operation: TraceSinkDirectory::Warehouse,
                path: root.clone(),
                source,
            }
        })?;
    }

    tracing::info!(
        catalog = %connect.catalog.redacted_for_logs(),
        warehouse = %connect.warehouse.summary_for_logs(),
        "connecting Iceberg SqlCatalog"
    );
    let iceberg = Arc::new(
        IcebergSink::connect(&connect)
            .await
            .map_err(TraceSinkRunError::Iceberg)?,
    );

    tracing::info!(
        catalog = %connect.catalog.redacted_for_logs(),
        warehouse = %connect.warehouse.summary_for_logs(),
        "Iceberg SqlCatalog ready"
    );

    let persisted = PersistedTraceSink::connect(
        &connect,
        iceberg.clone(),
        config.segment_projection_ttl_secs,
        config.segment_projection_gc_interval_secs,
    )
    .await
    .map_err(TraceSinkRunError::Persisted)?;

    persisted.start_background_tasks();

    tracing::info!(
        segment_projection_ttl_secs = config.segment_projection_ttl_secs,
        segment_projection_gc_interval_secs = config.segment_projection_gc_interval_secs,
        "trace segment projection TTL configured"
    );

    let store: Arc<dyn AuditSpanStore> = persisted;

    tracing::info!("trace projection store ready (plasm_trace_sink schema on Postgres)");
    let state = AppState::new(store);
    let app = router(state);

    let addr: SocketAddr = config
        .listen
        .parse()
        .map_err(TraceSinkRunError::ListenAddress)?;

    tracing::info!(%addr, "binding HTTP listener");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(TraceSinkRunError::Bind)?;

    tracing::info!(%addr, "serving HTTP (health GET /v1/health)");
    axum::serve(listener, app)
        .await
        .map_err(TraceSinkRunError::Serve)?;

    tracing::warn!("axum::serve returned Ok — this is unexpected for a long-running server");
    Ok(())
}
