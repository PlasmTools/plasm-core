//! Single-binary Plasm appliance: embedded [`plasm_agent_core`] kernel + optional Ratatui UI.
//! Remote operator/debug traffic stays on **`plasm`** (strict HTTP terminal client).

mod appliance_admin_bridge;
mod appliance_log;
mod appliance_mcp_admin;
mod appliance_mode;
mod appliance_oauth_admin;
mod boot;
mod mcp_cli;
mod oauth_cli;
mod oauth_upsert_wizard;
mod serve_ui_mode;
mod stderr_log;
mod tui;

use std::error::Error;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use plasm_agent::embedded_postgres::EmbeddedPostgresGuard;
use plasm_agent_core::mcp_host_bootstrap;
use plasm_agent_core::mcp_host_bootstrap::CatalogLoadOutcome;
use plasm_core::discovery::CgsCatalog;

#[derive(Parser, Debug)]
#[command(
    name = "plasm-server",
    version = env!("CARGO_PKG_VERSION"),
    about = "In-process Plasm MCP appliance + control-station TUI"
)]
struct RootCli {
    #[command(subcommand)]
    command: Option<TopCommand>,
    #[command(flatten)]
    serve: ServeCli,
}

#[derive(Subcommand, Debug)]
enum TopCommand {
    /// HTTP + MCP listeners + optional control-station TUI (same as default invocation).
    Serve {
        #[command(flatten)]
        inner: ServeCli,
    },
    /// MCP configuration for the OSS singleton (`project_mcp_*` + transport keys).
    Mcp(mcp_cli::McpCliRoot),
    /// Outbound OAuth providers (`oauth_provider_apps`) + device authorization helpers.
    Oauth(oauth_cli::OauthCliRoot),
}

#[derive(Parser, Debug, Clone)]
pub(crate) struct ServeCli {
    /// Appliance state root (default: `$PLASM_APPLIANCE_DIR` or `~/.plasm/appliance`).
    ///
    /// Always applied before boot: sets `PLASM_EMBEDDED_POSTGRES_DATA_DIR` to `{dir}/postgres`
    /// (pg-embed **reuses** an existing cluster there; keep only Postgres files under `postgres/`)
    /// and clears inherited `PGDATA`. Sets `PLASM_LOCAL_STATE_DIR` to `{dir}/local` when unset.
    /// Override with `--data-dir` or `PLASM_APPLIANCE_DIR` (same path as `install.sh`).
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// CGS schema path (exactly one of `--schema` or `--catalog-dir` required unless `--migrate-mcp-config-db`).
    ///
    /// When omitted, uses `{appliance}/catalogs` when that directory exists.
    #[arg(long, value_name = "PATH", group = "catalog")]
    schema: Option<PathBuf>,
    /// Compiled catalog artifact directory (JSON IL + manifests). Defaults to `{appliance}/catalogs`.
    #[arg(long, value_name = "DIR", group = "catalog")]
    catalog_dir: Option<PathBuf>,
    /// TCP listen host (default: `127.0.0.1`, or `0.0.0.0` in Kubernetes; see `PLASM_LISTEN_HOST`).
    #[arg(long, value_name = "HOST")]
    listen_host: Option<String>,
    /// TCP port for HTTP discovery/execute and MCP Streamable HTTP (`/mcp`) on **one** listener.
    #[arg(long, default_value_t = 3000)]
    port: u16,
    #[arg(long)]
    symbol_tuning: Option<String>,
    /// Run `project_mcp_*` sqlx migrations then exit (respects embedded Postgres env).
    #[arg(long)]
    migrate_mcp_config_db: bool,
    /// Headless: HTTP + MCP only; bootstrap and milestones on stderr (containers / systemd).
    #[arg(long, conflicts_with = "tui")]
    no_tui: bool,
    /// Force the Ratatui control station even when stdout or stdin is not a TTY.
    #[arg(long, conflicts_with = "no_tui")]
    tui: bool,
}

fn env_str_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

const DEFAULT_APPLIANCE_DIR_NAME: &str = ".plasm/appliance";
const DEFAULT_CATALOGS_DIR_NAME: &str = "catalogs";

/// OSS installer layout: `PLASM_APPLIANCE_DIR` or `~/.plasm/appliance` (see `install.sh`).
fn default_appliance_root() -> PathBuf {
    if let Ok(p) = std::env::var("PLASM_APPLIANCE_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(DEFAULT_APPLIANCE_DIR_NAME))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_APPLIANCE_DIR_NAME))
}

fn resolve_appliance_root(cli: &ServeCli) -> PathBuf {
    cli.data_dir.clone().unwrap_or_else(default_appliance_root)
}

/// Default `--catalog-dir` to `{appliance}/catalogs` when present.
fn apply_serve_cli_release_defaults(cli: &mut ServeCli) {
    if cli.migrate_mcp_config_db {
        return;
    }
    if cli.schema.is_some() || cli.catalog_dir.is_some() {
        return;
    }
    let catalogs = resolve_appliance_root(cli).join(DEFAULT_CATALOGS_DIR_NAME);
    if catalogs.is_dir() {
        cli.catalog_dir = Some(catalogs);
    }
}

/// Applies the appliance layout by setting process env before embedded Postgres or host bootstrap.
fn apply_appliance_layout_env_defaults(cli: &ServeCli) -> std::io::Result<()> {
    let root = resolve_appliance_root(cli);
    std::fs::create_dir_all(&root)?;
    // `--data-dir` owns the appliance tree: always pin embedded PG under `{root}/postgres`
    // so inherited PGDATA / PLASM_EMBEDDED_POSTGRES_DATA_DIR from the shell cannot leak in.
    let pg = root.join("postgres");
    std::fs::create_dir_all(&pg)?;
    std::env::set_var("PLASM_EMBEDDED_POSTGRES_DATA_DIR", pg.as_os_str());
    std::env::remove_var("PGDATA");
    // Inherited loopback DATABASE_URL ports (e.g. a prior appliance on 55432) must not steer
    // embedded pg-embed bind — listener port is chosen at autostart, then URLs are rewritten.
    if EmbeddedPostgresGuard::will_autostart_embedded_postgres() {
        for key in [
            "DATABASE_URL",
            "PLASM_MCP_CONFIG_DATABASE_URL",
            "PLASM_AUTH_STORAGE_URL",
        ] {
            std::env::remove_var(key);
        }
    }
    if !env_str_nonempty("PLASM_LOCAL_STATE_DIR") {
        let local = root.join("local");
        std::fs::create_dir_all(&local)?;
        std::env::set_var("PLASM_LOCAL_STATE_DIR", local.as_os_str());
    }
    Ok(())
}

/// Re-pin appliance Postgres paths and undo `.env` URLs that block embedded autostart (loaded after layout).
fn reconcile_appliance_db_env(cli: &ServeCli) {
    if EmbeddedPostgresGuard::embedded_postgres_explicitly_disabled() {
        return;
    }
    let root = resolve_appliance_root(cli);
    let pg = root.join("postgres");
    let _ = std::fs::create_dir_all(&pg);
    std::env::set_var("PLASM_EMBEDDED_POSTGRES_DATA_DIR", pg.as_os_str());
    std::env::remove_var("PGDATA");
    let any_db_url = env_str_nonempty("DATABASE_URL")
        || env_str_nonempty("PLASM_MCP_CONFIG_DATABASE_URL")
        || env_str_nonempty("PLASM_AUTH_STORAGE_URL");
    if cli.migrate_mcp_config_db
        || EmbeddedPostgresGuard::env_urls_skip_embedded_autostart()
        || any_db_url
    {
        EmbeddedPostgresGuard::clear_env_urls_blocking_embedded_autostart();
    }
}

/// Start embedded Postgres for `plasm-server mcp` / `oauth` / `migrate-db` when no external URL is set.
async fn ensure_cli_policy_store(
    cli: &ServeCli,
) -> Result<Option<EmbeddedPostgresGuard>, Box<dyn std::error::Error + Send + Sync>> {
    reconcile_appliance_db_env(cli);
    if !EmbeddedPostgresGuard::will_autostart_embedded_postgres() {
        if let Err(e) = ensure_local_auth_storage_encryption_key() {
            return Err(Box::new(std::io::Error::other(e)));
        }
        return Ok(None);
    }
    if plasm_agent_core::mcp_config_repository::mcp_config_database_url().is_some() {
        if let Err(e) = ensure_local_auth_storage_encryption_key() {
            return Err(Box::new(std::io::Error::other(e)));
        }
        return Ok(None);
    }
    let guard = EmbeddedPostgresGuard::try_start_from_env().await.map_err(
        |e| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(std::io::Error::other(e.to_string()))
        },
    )?;
    if let Err(e) = ensure_local_auth_storage_encryption_key() {
        return Err(Box::new(std::io::Error::other(e)));
    }
    Ok(guard)
}

fn policy_store_handoff_detail(
    attach: plasm_agent_core::mcp_host_bootstrap::McpPolicyAttachOutcome,
    repo_attached: bool,
) -> Option<crate::appliance_mode::PolicyStoreBootstrapDetail> {
    if repo_attached {
        return None;
    }
    crate::appliance_mode::PolicyStoreBootstrapDetail::from_attach(attach)
}

fn send_bootstrap_ui(
    ui_tx: Option<&crossbeam_channel::Sender<boot::BootstrapUiMsg>>,
    msg: boot::BootstrapUiMsg,
) {
    if let Some(t) = ui_tx {
        if t.send(msg).is_err() {
            tracing::warn!(
                target: "plasm_appliance_boot",
                "bootstrap UI channel closed; dropping message"
            );
        }
    }
}

fn redact_postgres_url_for_display(url: &str) -> String {
    let t = url.trim();
    if let Some(at) = t.find('@') {
        if let Some(scheme) = t.find("://") {
            let creds = &t[scheme + 3..at];
            if let Some(colon) = creds.find(':') {
                let user = &creds[..colon];
                return format!("{}://{}:***{}", &t[..scheme + 3], user, &t[at..]);
            }
        }
    }
    t.to_string()
}

const LOCAL_AUTH_STORAGE_KEY_RELATIVE_PATH: &str = "bootstrap-secrets/AUTH_STORAGE_ENCRYPTION_KEY";
const LOCAL_AUTH_JWT_SECRET_RELATIVE_PATH: &str = "bootstrap-secrets/PLASM_AUTH_JWT_SECRET";

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalAuthStorageKeyBootstrap {
    NotRequired,
    ProvidedByEnv,
    ManagedBySecretsDir,
    LoadedFromFile { path: PathBuf },
    GeneratedFile { path: PathBuf },
}

impl LocalAuthStorageKeyBootstrap {
    fn boot_detail(&self) -> Option<String> {
        match self {
            Self::LoadedFromFile { path } => Some(format!(
                "local auth storage key loaded from {}",
                path.display()
            )),
            Self::GeneratedFile { path } => Some(format!(
                "local auth storage key generated at {}",
                path.display()
            )),
            Self::ProvidedByEnv | Self::ManagedBySecretsDir | Self::NotRequired => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalAuthJwtSecretBootstrap {
    ProvidedByEnv,
    LoadedFromFile { path: PathBuf },
    GeneratedFile { path: PathBuf },
}

impl LocalAuthJwtSecretBootstrap {
    fn boot_detail(&self) -> Option<String> {
        match self {
            Self::LoadedFromFile { path } => Some(format!(
                "local JWT signing secret loaded from {}",
                path.display()
            )),
            Self::GeneratedFile { path } => Some(format!(
                "local JWT signing secret generated at {}",
                path.display()
            )),
            Self::ProvidedByEnv => None,
        }
    }
}

fn local_auth_jwt_secret_path() -> Option<PathBuf> {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|root| root.join(LOCAL_AUTH_JWT_SECRET_RELATIVE_PATH))
}

fn ensure_local_appliance_jwt_signing_secret() -> Result<LocalAuthJwtSecretBootstrap, String> {
    if env_str_nonempty("PLASM_AUTH_JWT_SECRET") {
        return Ok(LocalAuthJwtSecretBootstrap::ProvidedByEnv);
    }
    if plasm_agent_core::bootstrap_secrets::running_inside_kubernetes() {
        return Err(
            "PLASM_AUTH_JWT_SECRET is required in Kubernetes; set it via your deployment secrets."
                .to_string(),
        );
    }
    let Some(path) = local_auth_jwt_secret_path() else {
        return Err(
            "local appliance JWT bootstrap could not resolve a durable path; set PLASM_LOCAL_STATE_DIR, ensure HOME is set, or provide PLASM_AUTH_JWT_SECRET explicitly."
                .to_string(),
        );
    };
    let existed = path.exists();
    let secret = if existed {
        read_local_auth_storage_key(&path)?
    } else {
        let secret = generate_auth_storage_encryption_key();
        write_local_auth_storage_key(&path, &secret)?;
        read_local_auth_storage_key(&path)?
    };
    std::env::set_var("PLASM_AUTH_JWT_SECRET", &secret);
    Ok(if existed {
        LocalAuthJwtSecretBootstrap::LoadedFromFile { path }
    } else {
        LocalAuthJwtSecretBootstrap::GeneratedFile { path }
    })
}

fn auth_storage_uses_postgres() -> bool {
    env_str_nonempty("PLASM_AUTH_STORAGE_URL") || env_str_nonempty("DATABASE_URL")
}

fn local_auth_storage_key_path() -> Option<PathBuf> {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|root| root.join(LOCAL_AUTH_STORAGE_KEY_RELATIVE_PATH))
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0usize;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes.get(i + 1).copied();
        let b2 = bytes.get(i + 2).copied();
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        match (b1, b2) {
            (Some(v1), Some(v2)) => {
                out.push(TABLE[(((v1 & 0b0000_1111) << 2) | (v2 >> 6)) as usize] as char);
                out.push(TABLE[(v2 & 0b0011_1111) as usize] as char);
            }
            (Some(v1), None) => {
                out.push(TABLE[((v1 & 0b0000_1111) << 2) as usize] as char);
                out.push('=');
            }
            (None, None) => {
                out.push('=');
                out.push('=');
            }
            (None, Some(_)) => unreachable!(),
        }
        i += 3;
    }
    out
}

fn generate_auth_storage_encryption_key() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    encode_base64(&bytes)
}

fn validate_auth_storage_encryption_key() -> Result<(), String> {
    auth_framework::storage::StorageEncryption::new()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn invalid_local_auth_storage_key_message(path: &Path, err: &str) -> String {
    format!(
        "local appliance auth key file is invalid: {}: {err}\nDelete or replace that file to mint a fresh local key on next start. Warning: replacing it will orphan previously encrypted OAuth secrets and MCP API keys.",
        path.display()
    )
}

fn read_local_auth_storage_key(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "local appliance auth key file read failed: {}: {e}",
            path.display()
        )
    })?;
    let key = raw.trim().to_string();
    if key.is_empty() {
        return Err(format!(
            "local appliance auth key file is empty: {}\nDelete or replace that file to mint a fresh local key on next start. Warning: replacing it will orphan previously encrypted OAuth secrets and MCP API keys.",
            path.display()
        ));
    }
    Ok(key)
}

fn open_local_secret_file_for_write(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn write_local_auth_storage_key(path: &Path, key: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!(
            "local appliance auth key path has no parent directory: {}",
            path.display()
        ));
    };
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "local appliance auth key directory create failed: {}: {e}",
            parent.display()
        )
    })?;
    match open_local_secret_file_for_write(path) {
        Ok(mut file) => file.write_all(key.as_bytes()).map_err(|e| {
            format!(
                "local appliance auth key file write failed: {}: {e}",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!(
            "local appliance auth key file create failed: {}: {e}",
            path.display()
        )),
    }
}

fn ensure_local_auth_storage_encryption_key() -> Result<LocalAuthStorageKeyBootstrap, String> {
    if env_str_nonempty("AUTH_STORAGE_ENCRYPTION_KEY") {
        return Ok(LocalAuthStorageKeyBootstrap::ProvidedByEnv);
    }
    if env_str_nonempty("PLASM_SECRETS_DIR") {
        return Ok(LocalAuthStorageKeyBootstrap::ManagedBySecretsDir);
    }
    if plasm_agent_core::bootstrap_secrets::running_inside_kubernetes() {
        return Ok(LocalAuthStorageKeyBootstrap::NotRequired);
    }
    if !auth_storage_uses_postgres() {
        return Ok(LocalAuthStorageKeyBootstrap::NotRequired);
    }
    let Some(path) = local_auth_storage_key_path() else {
        return Err(
            "local appliance auth key bootstrap could not resolve a durable path; set PLASM_LOCAL_STATE_DIR, ensure HOME is set, or provide AUTH_STORAGE_ENCRYPTION_KEY explicitly."
                .to_string(),
        );
    };
    let existed = path.exists();
    let key = if existed {
        read_local_auth_storage_key(&path)?
    } else {
        let key = generate_auth_storage_encryption_key();
        write_local_auth_storage_key(&path, &key)?;
        read_local_auth_storage_key(&path)?
    };
    std::env::set_var("AUTH_STORAGE_ENCRYPTION_KEY", &key);
    if let Err(err) = validate_auth_storage_encryption_key() {
        return Err(invalid_local_auth_storage_key_message(&path, &err));
    }
    Ok(if existed {
        LocalAuthStorageKeyBootstrap::LoadedFromFile { path }
    } else {
        LocalAuthStorageKeyBootstrap::GeneratedFile { path }
    })
}

fn validate_serve_catalog(cli: &ServeCli) {
    if cli.migrate_mcp_config_db {
        return;
    }
    match (&cli.schema, &cli.catalog_dir) {
        (Some(_), None) | (None, Some(_)) => {}
        _ => {
            let root = resolve_appliance_root(cli);
            let catalogs = root.join(DEFAULT_CATALOGS_DIR_NAME);
            eprintln!(
                "plasm-server: pass exactly one of --schema PATH or --catalog-dir DIR (unless --migrate-mcp-config-db)"
            );
            eprintln!(
                "plasm-server: after install.sh, catalogs are usually at {}",
                catalogs.display()
            );
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_migrate_mcp_config_db(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut embedded_pg = EmbeddedPostgresGuard::try_start_from_env()
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { format!("{e}").into() })?;
    let Some(db_url) = plasm_agent_core::mcp_config_repository::mcp_config_database_url() else {
        shutdown_embedded_pg(&mut embedded_pg).await;
        eprintln!(
            "plasm-server: MCP migrate requires PLASM_MCP_CONFIG_DATABASE_URL, PLASM_AUTH_STORAGE_URL, or DATABASE_URL"
        );
        std::process::exit(1);
    };
    let migrate_result =
        plasm_agent_core::mcp_config_repository::McpConfigRepository::connect_and_migrate(&db_url)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("MCP config database migrate failed: {e}").into()
            });
    shutdown_embedded_pg(&mut embedded_pg).await;
    migrate_result?;
    tracing::info!("MCP configuration sqlx migrations applied successfully");
    Ok(())
}

enum BootstrapStopped {
    Cancelled,
    Fatal,
}

struct ApplianceBootstrapCoreResult {
    state: Arc<plasm_agent_core::server_state::PlasmHostState>,
    mcp_policy_attach: plasm_agent_core::mcp_host_bootstrap::McpPolicyAttachOutcome,
}

fn eprintln_exit_error(err: &dyn Error) {
    stderr_log::line(format!("plasm-server: error: {err}"));
    let mut src = err.source();
    while let Some(s) = src {
        stderr_log::line(format!("plasm-server:   caused by: {s}"));
        src = s.source();
    }
}

async fn recv_ui_event(
    rx: &crossbeam_channel::Receiver<boot::UiEvent>,
) -> Result<boot::UiEvent, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        match rx.try_recv() {
            Ok(e) => return Ok(e),
            Err(crossbeam_channel::TryRecvError::Empty) => {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                return Err("UI supervisor channel disconnected before RUN UI handshake".into());
            }
        }
    }
}

async fn shutdown_embedded_pg(slot: &mut Option<EmbeddedPostgresGuard>) {
    if let Some(g) = slot.take() {
        if let Err(e) = g.shutdown().await {
            tracing::warn!(error = %e, "embedded postgres: graceful shutdown failed");
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM signal handler");
        tokio::select! {
            res = tokio::signal::ctrl_c() => { let _ = res; }
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn synthesize_inner_argv(cli: &ServeCli) -> Vec<OsString> {
    let mut v = vec![OsString::from("plasm-server")];
    if let Some(ref p) = cli.schema {
        v.push(OsString::from("--schema"));
        v.push(p.as_os_str().to_owned());
    }
    if let Some(ref p) = cli.catalog_dir {
        v.push(OsString::from("--catalog-dir"));
        v.push(p.as_os_str().to_owned());
    }
    if let Some(ref st) = cli.symbol_tuning {
        v.push(OsString::from("--symbol-tuning"));
        v.push(OsString::from(st));
    }
    v.push(OsString::from("--http"));
    v.push(OsString::from("--mcp"));
    if let Some(ref host) = cli.listen_host {
        v.push(OsString::from("--listen-host"));
        v.push(OsString::from(host));
    }
    v.push(OsString::from("--port"));
    v.push(OsString::from(cli.port.to_string()));
    v
}

fn catalog_detail_line(cli: &ServeCli, outcome: &CatalogLoadOutcome) -> String {
    let loc = if cli.schema.is_some() {
        format!("--schema {}", outcome.schema_path)
    } else {
        format!("--catalog-dir {}", outcome.schema_path)
    };
    let n = outcome
        .prebuilt_registry
        .as_ref()
        .map(|r| r.list_entries().len())
        .unwrap_or(1);
    let entries_word = if n == 1 { "entry" } else { "entries" };
    format!("{loc}  |  {n} catalog {entries_word}")
}

async fn bootstrap_appliance_core(
    ui_tx: Option<&crossbeam_channel::Sender<boot::BootstrapUiMsg>>,
    cli: &ServeCli,
    argv: &[OsString],
    embedded_slot: &mut Option<EmbeddedPostgresGuard>,
    boot_cancel: &AtomicBool,
) -> Result<ApplianceBootstrapCoreResult, BootstrapStopped> {
    let send = |msg: boot::BootstrapUiMsg| send_bootstrap_ui(ui_tx, msg);

    let mirror_boot_stderr = ui_tx.is_none()
        || matches!(
            std::env::var("PLASM_APPLIANCE_BOOT_TRACE_STDERR").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        );

    // Ensures operators see failures on stderr and in logs (the BOOT TUI alone is easy to miss).
    let report_fatal = |msg: &str| {
        tracing::error!(target: "plasm_appliance_boot", error = %msg, "bootstrap failed");
        if mirror_boot_stderr {
            stderr_log::line(format!("plasm-server: {msg}"));
        }
    };

    let phase_line = |label: &str| {
        tracing::info!(target: "plasm_appliance_boot", "phase: {}", label);
        if mirror_boot_stderr {
            stderr_log::line(format!("[plasm-server] phase: {label}"));
        }
    };

    let check_cancel = || -> Result<(), BootstrapStopped> {
        if boot_cancel.load(Ordering::SeqCst) {
            send(boot::BootstrapUiMsg::Shutdown);
            Err(BootstrapStopped::Cancelled)
        } else {
            Ok(())
        }
    };

    send(boot::BootstrapUiMsg::PhaseEnter(0));
    phase_line("load catalog");

    let argv_owned: Vec<OsString> = argv.to_vec();
    let ui_detail_tx = ui_tx.cloned();
    let catalog_task = tokio::task::spawn_blocking(move || {
        use plasm_agent_core::error::AgentError;

        let push_detail = |line: &str| {
            if let Some(ref t) = ui_detail_tx {
                let _ = t.send(boot::BootstrapUiMsg::DetailPush(line.to_string()));
            }
        };

        push_detail("parsing inner MCP argv…");
        let pre_matches = mcp_host_bootstrap::preparse_mcp_command()
            .try_get_matches_from(&argv_owned)
            .map_err(|e| AgentError::Argument(format!("inner argv parse (pre): {e:#}")))?;

        let mut load_prog = |s: &str| push_detail(s);
        let catalog_outcome = mcp_host_bootstrap::load_catalog_for_mcp_server_with_progress(
            &pre_matches,
            true,
            &mut load_prog,
        )?;

        let mut validate_prog = |s: &str| push_detail(s);
        mcp_host_bootstrap::validate_catalog_templates_with_progress(
            &catalog_outcome,
            &mut validate_prog,
        )?;

        push_detail("building MCP CLI surface…");
        let app = plasm_agent_core::cli_builder::build_app(
            &catalog_outcome.cgs,
            plasm_agent_core::AgentCliSurface::McpServer,
        );
        push_detail("matching full argv against catalog CLI…");
        let matches = app
            .try_get_matches_from(&argv_owned)
            .map_err(|e| AgentError::Argument(format!("inner argv parse (full CLI): {e:#}")))?;
        Ok::<_, AgentError>((catalog_outcome, matches))
    });

    let (catalog_outcome, matches) = match catalog_task.await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
        Err(e) => {
            let msg = format!("catalog load task failed: {e}");
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
    };

    send(boot::BootstrapUiMsg::Detail(catalog_detail_line(
        cli,
        &catalog_outcome,
    )));
    send(boot::BootstrapUiMsg::PhaseDone(0));
    if mirror_boot_stderr {
        stderr_log::line(format!(
            "[plasm-server] catalog: {}",
            catalog_detail_line(cli, &catalog_outcome)
        ));
    }
    if mirror_boot_stderr {
        if let Some(ref pd) = cli.catalog_dir {
            match std::fs::canonicalize(pd) {
                Ok(abs) => stderr_log::line(format!(
                    "[plasm-server] --catalog-dir resolves to {}",
                    abs.display()
                )),
                Err(e) => stderr_log::line(format!(
                    "[plasm-server] --catalog-dir {:?} does not resolve ({e}); relative paths depend on the current working directory",
                    pd
                )),
            }
        }
    }
    check_cancel()?;

    send(boot::BootstrapUiMsg::PhaseEnter(1));
    phase_line("validate templates");
    send(boot::BootstrapUiMsg::PhaseDone(1));
    check_cancel()?;

    if EmbeddedPostgresGuard::will_autostart_embedded_postgres() {
        send(boot::BootstrapUiMsg::PhaseEnter(2));
        phase_line("embedded PostgreSQL");
        match EmbeddedPostgresGuard::try_start_from_env().await {
            Ok(eg) => {
                *embedded_slot = eg;
                send(boot::BootstrapUiMsg::Detail(
                    "embedded PostgreSQL ready".into(),
                ));
                if let Ok(url) = std::env::var("DATABASE_URL") {
                    send(boot::BootstrapUiMsg::Detail(format!(
                        "postgres listener: {}",
                        redact_postgres_url_for_display(&url)
                    )));
                }
                send(boot::BootstrapUiMsg::PhaseDone(2));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                report_fatal(&msg);
                send(boot::BootstrapUiMsg::Fatal(msg));
                return Err(BootstrapStopped::Fatal);
            }
        }
    } else {
        let reason = EmbeddedPostgresGuard::embedded_autostart_skip_reason()
            .unwrap_or("using external Postgres URLs")
            .to_string();
        send(boot::BootstrapUiMsg::PhaseSkip(2, reason.clone()));
        phase_line("embedded PostgreSQL — skipped");
    }
    check_cancel()?;

    let local_key_bootstrap = match ensure_local_auth_storage_encryption_key() {
        Ok(state) => {
            if let Some(detail) = state.boot_detail() {
                send(boot::BootstrapUiMsg::Detail(detail));
            }
            state
        }
        Err(msg) => {
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
    };
    let local_jwt_bootstrap = match ensure_local_appliance_jwt_signing_secret() {
        Ok(state) => {
            if let Some(detail) = state.boot_detail() {
                send(boot::BootstrapUiMsg::Detail(detail));
            }
            state
        }
        Err(msg) => {
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
    };

    send(boot::BootstrapUiMsg::PhaseEnter(3));
    phase_line("build engine + host state");
    send(boot::BootstrapUiMsg::Detail(
        "bootstrap_plasm_host_state_oss".into(),
    ));
    let host_bootstrap = match mcp_host_bootstrap::bootstrap_plasm_host_state_oss(
        &matches,
        &catalog_outcome,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("{e:#}");
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
    };
    let mcp_policy_attach = host_bootstrap.mcp_policy_attach;
    let mut app_state = host_bootstrap.state;
    if let plasm_agent_core::mcp_host_bootstrap::McpPolicyAttachOutcome::Failed(ref e) =
        mcp_policy_attach
    {
        send(boot::BootstrapUiMsg::DetailPush(format!(
            "project_mcp_* connect/migrate failed: {e:#}"
        )));
    }
    send(boot::BootstrapUiMsg::PhaseDone(3));
    check_cancel()?;

    let mcp_policy_attach = match crate::appliance_mode::evaluate_policy_bootstrap_gate(
        crate::appliance_mode::appliance_policy_requirement(cli),
        &app_state,
        mcp_policy_attach,
    ) {
        crate::appliance_mode::BootstrapGateOutcome::Proceed(attach) => attach,
        crate::appliance_mode::BootstrapGateOutcome::Fatal(detail) => {
            for line in detail.display_lines() {
                send(boot::BootstrapUiMsg::DetailPush(line));
            }
            let msg = detail.fatal_message();
            report_fatal(&msg);
            send(boot::BootstrapUiMsg::Fatal(msg));
            return Err(BootstrapStopped::Fatal);
        }
    };
    if app_state.mcp_config_repository().is_some() {
        phase_line("project_mcp_* policy store connected (migrations applied)");
        send(boot::BootstrapUiMsg::Detail(
            "project_mcp_* policy store connected (migrations applied)".into(),
        ));
    }

    send(boot::BootstrapUiMsg::PhaseEnter(4));
    phase_line("attach OSS extensions");
    send(boot::BootstrapUiMsg::Detail(
        "OAuth / MCP policy / discovery embeddings (host bootstrap)".into(),
    ));
    if let Err(e) = mcp_host_bootstrap::ensure_auth_framework_on_host(&mut app_state).await {
        let mut msg = format!("auth-framework init failed: {e}");
        if let LocalAuthStorageKeyBootstrap::LoadedFromFile { path }
        | LocalAuthStorageKeyBootstrap::GeneratedFile { path } = &local_key_bootstrap
        {
            msg.push_str(&format!(
                "\nLocal appliance auth key file: {}\nIf you delete or replace that file, previously encrypted OAuth secrets and MCP API keys will become unreadable.",
                path.display()
            ));
        }
        if let LocalAuthJwtSecretBootstrap::LoadedFromFile { path }
        | LocalAuthJwtSecretBootstrap::GeneratedFile { path } = &local_jwt_bootstrap
        {
            msg.push_str(&format!(
                "\nLocal appliance JWT signing secret file: {}\nIf you replace it, previously minted incoming-auth tokens become invalid.",
                path.display()
            ));
        }
        report_fatal(&msg);
        send(boot::BootstrapUiMsg::Fatal(msg));
        return Err(BootstrapStopped::Fatal);
    }
    send(boot::BootstrapUiMsg::Detail(
        "auth-framework + encrypted auth storage attached".into(),
    ));
    let oauth_link_catalog =
        Arc::new(plasm_agent_core::oauth_link_catalog::OauthLinkCatalog::from_env());
    if let Some(settings) =
        plasm_agent_core::oauth_provider_pull::OauthProviderPullSettings::from_env()
    {
        match plasm_agent_core::oauth_provider_pull::init_oauth_provider_pull_from_postgres(
            oauth_link_catalog.clone(),
            settings,
        )
        .await
        {
            plasm_agent_core::oauth_provider_pull::OauthProviderPullInitOutcome::ConnectFailed {
                error,
            } => {
                tracing::warn!(
                    error = %error,
                    "oauth_provider_pull: could not connect; runtime catalog uses file / upsert only"
                );
                send(boot::BootstrapUiMsg::Detail(format!(
                    "oauth provider pull connect failed: {error}"
                )));
            }
            plasm_agent_core::oauth_provider_pull::OauthProviderPullInitOutcome::Ran {
                periodic_spawned,
                ..
            } => {
                tracing::debug!(
                    periodic_spawned,
                    "oauth_provider_pull: startup refresh complete"
                );
                let detail = if periodic_spawned {
                    "oauth provider pull: startup refresh complete (periodic refresh active)"
                } else {
                    "oauth provider pull: startup refresh complete"
                };
                send(boot::BootstrapUiMsg::Detail(detail.into()));
            }
        }
    }
    let auth_storage = app_state
        .oss
        .auth_storage
        .clone()
        .expect("auth storage after ensure_auth_framework_on_host");
    let outbound_secret_provider = Arc::new(
        plasm_agent_core::outbound_secret_provider::AgentOutboundSecretProvider::new(
            auth_storage,
            oauth_link_catalog.clone(),
        ),
    );
    app_state.oss.oauth_link_catalog = Some(oauth_link_catalog);
    app_state.oss.outbound_secret_provider =
        Some(outbound_secret_provider as Arc<dyn plasm_runtime::SecretProvider>);
    send(boot::BootstrapUiMsg::PhaseDone(4));
    check_cancel()?;

    Ok(ApplianceBootstrapCoreResult {
        state: Arc::new(app_state),
        mcp_policy_attach,
    })
}

async fn join_ui_thread(
    ui_handle: std::thread::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let join_out = tokio::task::spawn_blocking(move || ui_handle.join())
        .await
        .map_err(|e| format!("ui join wrapper: {e}"))?;
    match join_out {
        Ok(ui_res) => ui_res.map_err(|e| -> Box<dyn std::error::Error> { e }),
        Err(_) => Err("plasm-server: UI thread panicked".into()),
    }
}

type BlockingUiJoinOut = Result<
    Result<Result<(), Box<dyn std::error::Error + Send + Sync>>, Box<dyn std::any::Any + Send>>,
    tokio::task::JoinError,
>;

fn init_appliance_runtime_headless() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::var_os("PLASM_APPLIANCE_DIAG_LOG") {
        Some(ref raw) if !raw.is_empty() => {
            let path = Path::new(raw);
            match appliance_log::ApplianceDiagFileMakeWriter::open(path) {
                Ok(w) => plasm_agent_core::init_agent_runtime_with_fmt_writer(w),
                Err(e) => {
                    stderr_log::line(format!(
                        "plasm-server: could not open PLASM_APPLIANCE_DIAG_LOG ({}): {e}; continuing without diag file sink",
                        path.display()
                    ));
                    plasm_agent_core::init_agent_runtime()
                }
            }
        }
        _ => plasm_agent_core::init_agent_runtime(),
    }
}

fn flatten_blocking_ui_join(
    res: BlockingUiJoinOut,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match res {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err(Box::<dyn std::error::Error + Send + Sync>::from(
            "plasm-server: UI thread panicked",
        )),
        Err(e) => Err(Box::new(e)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut root = RootCli::parse();
    apply_serve_cli_release_defaults(&mut root.serve);
    plasm_agent_core::dotenv_safe::load_from_cwd_parents();
    if let Err(e) = apply_appliance_layout_env_defaults(&root.serve) {
        eprintln!("plasm-server: appliance data directory: {e}");
        std::process::exit(1);
    }

    if root.serve.migrate_mcp_config_db && root.command.is_none() {
        if let Err(e) = plasm_agent_core::init_agent_runtime() {
            eprintln_exit_error(&*e);
            return Err(e);
        }
        let _cli_pg = ensure_cli_policy_store(&root.serve).await.map_err(|e| {
            eprintln_exit_error(&*e);
            Box::new(std::io::Error::other(format!("{e}"))) as Box<dyn std::error::Error>
        })?;
        if let Err(e) = run_migrate_mcp_config_db().await {
            eprintln_exit_error(&*e);
            return Err(Box::new(std::io::Error::other(format!("{e}"))));
        }
        return Ok(());
    }

    let cli = match root.command {
        Some(TopCommand::Mcp(mcp)) => {
            if let Err(e) = plasm_agent_core::init_agent_runtime() {
                eprintln_exit_error(&*e);
                return Err(e);
            }
            let _cli_pg = ensure_cli_policy_store(&root.serve).await.map_err(|e| {
                eprintln_exit_error(&*e);
                Box::new(std::io::Error::other(format!("{e}"))) as Box<dyn std::error::Error>
            })?;
            if let Err(e) = mcp_cli::run_mcp(mcp).await {
                eprintln_exit_error(&*e);
                return Err(Box::new(std::io::Error::other(format!("{e}"))));
            }
            return Ok(());
        }
        Some(TopCommand::Oauth(oauth)) => {
            if let Err(e) = plasm_agent_core::init_agent_runtime() {
                eprintln_exit_error(&*e);
                return Err(e);
            }
            let _cli_pg = ensure_cli_policy_store(&root.serve).await.map_err(|e| {
                eprintln_exit_error(&*e);
                Box::new(std::io::Error::other(format!("{e}"))) as Box<dyn std::error::Error>
            })?;
            if let Err(e) = oauth_cli::run_oauth(oauth).await {
                eprintln_exit_error(&*e);
                return Err(Box::new(std::io::Error::other(format!("{e}"))));
            }
            return Ok(());
        }
        Some(TopCommand::Serve { inner }) => inner,
        None => root.serve,
    };

    validate_serve_catalog(&cli);

    let use_tui = serve_ui_mode::serve_use_tui(&cli);
    if !use_tui && !cli.no_tui {
        eprintln!(
            "plasm-server: headless mode (stdout/stdin not a TTY); pass --tui to force the control station"
        );
    }

    let argv = synthesize_inner_argv(&cli);
    let mut embedded_pg: Option<EmbeddedPostgresGuard> = None;

    let (appliance_log_tx, mut appliance_log_rx) = if !use_tui {
        if let Err(e) = init_appliance_runtime_headless() {
            eprintln_exit_error(&*e);
            return Err(e);
        }
        reconcile_appliance_db_env(&cli);
        (None, None)
    } else {
        let (tx, rx) = crossbeam_channel::bounded::<appliance_log::ApplianceLogEntry>(
            appliance_log::APPLIANCE_LOG_CHANNEL_CAP,
        );
        // Telemetry init (`plasm_otel`, OTLP, etc.) runs on the main thread. Do **not** install it
        // before the boot UI thread starts: PTY-based tests (and humans) otherwise see a silent
        // terminal while a PTY harness blocks on the first read with no timeout progress.
        (Some(tx), Some(rx))
    };

    let running = Arc::new(AtomicBool::new(true));
    let listen = plasm_agent_core::listen_endpoint::TcpListenEndpoint::from_cli(
        cli.listen_host.as_deref(),
        cli.port,
    )
    .map_err(|msg| {
        let err = std::io::Error::new(std::io::ErrorKind::InvalidInput, msg);
        eprintln_exit_error(&err);
        Box::new(err) as Box<dyn std::error::Error>
    })?;

    let ui_result: Result<(), Box<dyn std::error::Error + Send + Sync>> = if !use_tui {
        let boot_cancel = AtomicBool::new(false);
        let bootstrap_out = tokio::select! {
            _ = shutdown_signal() => {
                shutdown_embedded_pg(&mut embedded_pg).await;
                stderr_log::line(
                    "[plasm-server] bootstrap interrupted (Ctrl+C or SIGTERM) — headless mode",
                );
                return Ok(());
            }
            out = bootstrap_appliance_core(None, &cli, &argv, &mut embedded_pg, &boot_cancel) => out,
        };
        match bootstrap_out {
            Ok(bootstrap) => {
                let state = bootstrap.state;
                tracing::info!(target: "plasm_appliance_boot", "phase: bind HTTP+MCP listener");
                stderr_log::line("[plasm-server] phase: bind HTTP+MCP listener");
                let listener = match listen.bind_tcp_listener().await {
                    Ok(l) => l,
                    Err(e) => {
                        let msg = format!("listen bind failed on {}: {e:#}", listen.display_addr());
                        stderr_log::line(format!("plasm-server: {msg}"));
                        return Err(msg.into());
                    }
                };
                stderr_log::line(format!(
                    "[plasm-server] HTTP+MCP bound on {}",
                    listener.local_addr()?
                ));

                tracing::info!(target: "plasm_appliance_boot", "phase: serve unified listener");
                stderr_log::line("[plasm-server] phase: serve unified listener");

                let state_srv = (*state).clone();
                let unified_srv = tokio::spawn(async move {
                    plasm_agent_core::http::serve_discovery_execute_and_mcp_unified(
                        listener,
                        state_srv,
                        plasm_agent_core::http::DiscoveryHttpServeOpts::default(),
                    )
                    .await
                    .map_err(|e| format!("unified server error: {e:#}"))
                });

                tokio::task::yield_now().await;
                if unified_srv.is_finished() {
                    let err = match unified_srv.await {
                        Ok(Ok(())) => "unified listener exited immediately after bind".to_string(),
                        Ok(Err(s)) => s,
                        Err(e) => format!("unified server task join error: {e}"),
                    };
                    stderr_log::line(format!("[plasm-server] fatal: {err}"));
                    shutdown_embedded_pg(&mut embedded_pg).await;
                    return Err(err.into());
                }

                tracing::info!(target: "plasm_appliance_boot", "phase: headless listener running");
                stderr_log::line("[plasm-server] phase: headless listener running");

                tokio::pin!(unified_srv);

                let headless_supervised = async {
                    tokio::select! {
                        _ = shutdown_signal() => {
                            unified_srv.abort();
                            let _ = unified_srv.await;
                            stderr_log::line("[plasm-server] shutdown: signal received");
                            Ok(())
                        }
                        res = &mut unified_srv => {
                            match res {
                                Ok(Ok(())) => {
                                    let msg = "HTTP+MCP listener stopped while appliance should keep running";
                                    stderr_log::line(format!("[plasm-server] fatal: {msg}"));
                                    Err(Box::<dyn std::error::Error + Send + Sync>::from(msg))
                                }
                                Ok(Err(s)) => {
                                    stderr_log::line(format!("[plasm-server] fatal: {s}"));
                                    Err(s.into())
                                }
                                Err(e) if e.is_cancelled() => Ok(()),
                                Err(e) => Err(Box::<dyn std::error::Error + Send + Sync>::from(e)),
                            }
                        }
                    }
                };

                headless_supervised.await
            }
            Err(BootstrapStopped::Cancelled) => Ok(()),
            Err(BootstrapStopped::Fatal) => {
                shutdown_embedded_pg(&mut embedded_pg).await;
                std::process::exit(1);
            }
        }
    } else {
        let (tx, rx) = crossbeam_channel::bounded::<boot::BootstrapUiMsg>(64);
        let (ui_evt_tx, ui_evt_rx) = crossbeam_channel::bounded::<boot::UiEvent>(4);
        let boot_cancel = Arc::new(AtomicBool::new(false));
        let ui_running = Arc::clone(&running);
        let ui_boot_cancel = Arc::clone(&boot_cancel);
        let log_rx_for_ui = appliance_log_rx
            .take()
            .expect("TUI mode must have appliance log receiver");
        let log_tx = appliance_log_tx
            .as_ref()
            .expect("TUI mode must have appliance log sender")
            .clone();

        // Boot UI first so the PTY sees immediate Crossterm output; then install tracing/OTLP.
        let listen_for_boot = listen.clone();
        let ui_handle = std::thread::spawn(move || {
            boot::run_appliance_shell(
                rx,
                ui_running,
                ui_boot_cancel,
                Some(ui_evt_tx),
                listen_for_boot,
                Some(log_rx_for_ui),
            )
        });

        let diag_path = std::env::var_os("PLASM_APPLIANCE_DIAG_LOG")
            .filter(|raw| !raw.is_empty())
            .map(|raw| Path::new(&raw).to_path_buf());
        let appliance_log_writer = match diag_path.as_deref() {
            Some(path) => match appliance_log::appliance_fmt_make_writer(Some(path)) {
                Ok(w) => w,
                Err(e) => {
                    stderr_log::line(format!(
                        "plasm-server: could not open PLASM_APPLIANCE_DIAG_LOG ({}): {e}; continuing without diag file sink",
                        path.display()
                    ));
                    appliance_log::ApplianceFmtMakeWriter::Sink
                }
            },
            None => appliance_log::ApplianceFmtMakeWriter::Sink,
        };
        let tui_capture = Some(appliance_log::appliance_tui_callback(log_tx));

        if let Err(e) = plasm_agent_core::init_agent_runtime_with_appliance_logs(
            appliance_log_writer,
            tui_capture,
        ) {
            let _ = tx.send(boot::BootstrapUiMsg::Fatal(format!("{e:#}")));
            eprintln_exit_error(&*e);
            if let Err(je) = join_ui_thread(ui_handle).await {
                eprintln_exit_error(&*je);
            }
            return Err(e);
        }
        reconcile_appliance_db_env(&cli);

        let tx_ref = tx.clone();
        let bootstrap_out = tokio::select! {
            _ = shutdown_signal() => {
                boot_cancel.store(true, Ordering::SeqCst);
                drop(tx);
                shutdown_embedded_pg(&mut embedded_pg).await;
                if let Err(e) = join_ui_thread(ui_handle).await {
                    eprintln_exit_error(&*e);
                }
                stderr_log::line(
                    "[plasm-server] bootstrap interrupted (Ctrl+C or SIGTERM) — release terminal",
                );
                return Ok(());
            }
            out = bootstrap_appliance_core(
                Some(&tx_ref),
                &cli,
                &argv,
                &mut embedded_pg,
                boot_cancel.as_ref(),
            ) => out,
        };

        let bootstrap_out = match bootstrap_out {
            Ok(s) => Ok(s),
            Err(BootstrapStopped::Cancelled) => {
                shutdown_embedded_pg(&mut embedded_pg).await;
                if let Err(e) = join_ui_thread(ui_handle).await {
                    eprintln_exit_error(&*e);
                }
                return Ok(());
            }
            Err(BootstrapStopped::Fatal) => {
                shutdown_embedded_pg(&mut embedded_pg).await;
                if let Err(e) = join_ui_thread(ui_handle).await {
                    eprintln_exit_error(&*e);
                }
                std::process::exit(1);
            }
        };

        let bootstrap = match bootstrap_out {
            Ok(s) => s,
            Err(BootstrapStopped::Cancelled | BootstrapStopped::Fatal) => unreachable!(),
        };

        let state = bootstrap.state;
        let mcp_policy_attach = bootstrap.mcp_policy_attach;

        let _ = tx.send(boot::BootstrapUiMsg::PhaseEnter(5));
        tracing::info!(target: "plasm_appliance_boot", "phase: bind HTTP+MCP listener");
        let _ = tx.send(boot::BootstrapUiMsg::Detail(format!(
            "binding HTTP+MCP {}",
            listen.display_addr()
        )));

        let listener = match listen.bind_tcp_listener().await {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("listen bind failed on {}: {e:#}", listen.display_addr());
                let _ = tx.send(boot::BootstrapUiMsg::Fatal(msg.clone()));
                shutdown_embedded_pg(&mut embedded_pg).await;
                if let Err(je) = join_ui_thread(ui_handle).await {
                    eprintln_exit_error(&*je);
                }
                return Err(msg.into());
            }
        };
        let bound_port = listener.local_addr()?.port();
        let _ = tx.send(boot::BootstrapUiMsg::PhaseDone(5));

        let _ = tx.send(boot::BootstrapUiMsg::PhaseEnter(6));
        tracing::info!(target: "plasm_appliance_boot", "phase: start unified HTTP+MCP listener");
        let _ = tx.send(boot::BootstrapUiMsg::Detail(format!(
            "routes on {}  (MCP /mcp)",
            listen.display_addr()
        )));

        let log_tx_unified = appliance_log_tx.clone();
        let state_unified = (*state).clone();
        let unified_srv = tokio::spawn(async move {
            if let Some(ref ltx) = log_tx_unified {
                let help = plasm_agent_core::http::format_http_route_help(bound_port);
                appliance_log::push_block(ltx, &help);
            }
            plasm_agent_core::http::serve_discovery_execute_and_mcp_unified(
                listener,
                state_unified,
                plasm_agent_core::http::DiscoveryHttpServeOpts {
                    emit_stderr_route_help: false,
                },
            )
            .await
            .map_err(|e| format!("unified server error: {e:#}"))
        });

        tokio::task::yield_now().await;
        if unified_srv.is_finished() {
            running.store(false, Ordering::SeqCst);
            let err = match unified_srv.await {
                Ok(Ok(())) => "unified listener exited before RUN handshake".to_string(),
                Ok(Err(s)) => s,
                Err(e) => format!("unified server task join error: {e}"),
            };
            let _ = tx.send(boot::BootstrapUiMsg::Fatal(err.clone()));
            shutdown_embedded_pg(&mut embedded_pg).await;
            let _ = join_ui_thread(ui_handle).await;
            return Err(err.into());
        }

        let _ = tx.send(boot::BootstrapUiMsg::PhaseDone(6));

        let admin_bridge = crate::appliance_admin_bridge::spawn_admin_router(Arc::clone(&state));
        tracing::info!(target: "plasm_appliance_boot", "phase: run UI handoff");
        stderr_log::line("[plasm-server] bootstrap: sent RUN handoff to UI thread");
        let policy_store_detail =
            policy_store_handoff_detail(mcp_policy_attach, state.mcp_config_repository().is_some());
        send_bootstrap_ui(
            Some(&tx),
            boot::BootstrapUiMsg::Running(boot::RunningHandoff {
                state: Arc::clone(&state),
                admin_bridge,
                policy_store_detail,
            }),
        );

        tokio::select! {
            _ = shutdown_signal() => {
                running.store(false, Ordering::SeqCst);
                unified_srv.abort();
                shutdown_embedded_pg(&mut embedded_pg).await;
                if let Err(e) = join_ui_thread(ui_handle).await {
                    eprintln_exit_error(&*e);
                }
                stderr_log::line(
                    "[plasm-server] shutdown during RUN UI handshake (Ctrl+C or SIGTERM)",
                );
                return Ok(());
            }
            ev = tokio::time::timeout(Duration::from_secs(120), recv_ui_event(&ui_evt_rx)) => {
                match ev {
                    Ok(Ok(boot::UiEvent::RunEntered)) => {
                        tracing::info!(
                            target: "plasm_appliance_boot",
                            "RUN UI RunEntered received (supervisor)"
                        );
                    }
                    Ok(Err(e)) => {
                        running.store(false, Ordering::SeqCst);
                        unified_srv.abort();
                        let _ = join_ui_thread(ui_handle).await;
                        return Err(e as Box<dyn std::error::Error>);
                    }
                    Err(_) => {
                        running.store(false, Ordering::SeqCst);
                        boot_cancel.store(true, Ordering::SeqCst);
                        unified_srv.abort();
                        tracing::error!(
                            target: "plasm_appliance_boot",
                            "RUN UI RunEntered not observed within 120s"
                        );
                        stderr_log::line(
                            "[plasm-server] fatal: timeout waiting for RUN UI RunEntered (120s)",
                        );
                        match tokio::time::timeout(Duration::from_secs(5), join_ui_thread(ui_handle))
                            .await
                        {
                            Ok(Ok(())) => {}
                            _ => stderr_log::line(
                                "[plasm-server] warning: UI thread did not exit within 5s after RUN handshake timeout",
                            ),
                        }
                        return Err("timeout waiting for RUN UI RunEntered (120s)".into());
                    }
                }
            }
        };

        let _ = tx.send(boot::BootstrapUiMsg::PhaseEnter(7));
        let _ = tx.send(boot::BootstrapUiMsg::PhaseDone(7));
        tracing::info!(
            target: "plasm_appliance_boot",
            "phase: control station ready"
        );

        let run_ui = Arc::clone(&running);
        let ui_blocking = tokio::task::spawn_blocking(move || ui_handle.join());

        tokio::pin!(ui_blocking);
        tokio::pin!(unified_srv);

        let supervised = async {
            tokio::select! {
                _ = shutdown_signal() => {
                    run_ui.store(false, Ordering::SeqCst);
                    unified_srv.abort();
                    let ui_j = flatten_blocking_ui_join(ui_blocking.await);
                    let _ = unified_srv.await;
                    stderr_log::line("[plasm-server] shutdown: signal received");
                    ui_j
                }
                join_res = &mut ui_blocking => {
                    unified_srv.abort();
                    let _ = unified_srv.await;
                    stderr_log::line("[plasm-server] shutdown: control station closed");
                    flatten_blocking_ui_join(join_res)
                }
                res = &mut unified_srv => {
                    run_ui.store(false, Ordering::SeqCst);
                    let ui_j = flatten_blocking_ui_join(ui_blocking.await);
                    match res {
                        Ok(Ok(())) => {
                            let msg = "HTTP+MCP listener stopped while appliance should keep running";
                            stderr_log::line(format!("[plasm-server] fatal: {msg}"));
                            match ui_j {
                                Ok(()) => Err(Box::<dyn std::error::Error + Send + Sync>::from(msg)),
                                Err(e) => Err(e),
                            }
                        }
                        Ok(Err(s)) => {
                            stderr_log::line(format!("[plasm-server] fatal: {s}"));
                            match ui_j {
                                Ok(()) => Err(s.into()),
                                Err(e) => Err(e),
                            }
                        }
                        Err(e) if e.is_cancelled() => ui_j,
                        Err(e) => match ui_j {
                            Ok(()) => Err(Box::<dyn std::error::Error + Send + Sync>::from(e)),
                            Err(uie) => Err(uie),
                        },
                    }
                }
            }
        };

        supervised.await
    };

    shutdown_embedded_pg(&mut embedded_pg).await;

    match ui_result {
        Ok(()) => {
            stderr_log::line("[plasm-server] exited cleanly");
            Ok(())
        }
        Err(e) => {
            eprintln_exit_error(&*e);
            stderr_log::line("[plasm-server] exiting with error");
            Err(Box::new(std::io::Error::other(format!("{e}"))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn clear_test_env() {
        for key in [
            "AUTH_STORAGE_ENCRYPTION_KEY",
            "PLASM_SECRETS_DIR",
            "PLASM_AUTH_STORAGE_URL",
            "DATABASE_URL",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn reconcile_appliance_db_env_clears_dotenv_external_database_url() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "DATABASE_URL",
            "PLASM_MCP_CONFIG_DATABASE_URL",
            "PLASM_AUTH_STORAGE_URL",
            "PLASM_EMBEDDED_POSTGRES",
            "PLASM_EMBEDDED_POSTGRES_DATA_DIR",
            "PGDATA",
        ]);
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(
            "DATABASE_URL",
            "postgresql://user:pass@db.example.com:5432/plasm",
        );
        let cli = ServeCli {
            data_dir: Some(temp.path().to_path_buf()),
            schema: None,
            catalog_dir: None,
            listen_host: None,
            port: 3000,
            symbol_tuning: None,
            migrate_mcp_config_db: false,
            no_tui: true,
            tui: false,
        };
        reconcile_appliance_db_env(&cli);
        assert!(
            EmbeddedPostgresGuard::will_autostart_embedded_postgres(),
            "external DATABASE_URL from dotenv must not block embedded autostart"
        );
        assert!(
            !env_str_nonempty("DATABASE_URL"),
            "reconcile should clear inherited DATABASE_URL for appliance embedded mode"
        );
        assert!(
            std::env::var("PLASM_EMBEDDED_POSTGRES_DATA_DIR")
                .unwrap()
                .contains("postgres"),
            "reconcile should pin embedded data dir under appliance root"
        );
    }

    #[test]
    fn local_auth_storage_key_bootstrap_skips_without_durable_storage() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "AUTH_STORAGE_ENCRYPTION_KEY",
            "PLASM_SECRETS_DIR",
            "PLASM_AUTH_STORAGE_URL",
            "DATABASE_URL",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ]);
        clear_test_env();

        assert_eq!(
            ensure_local_auth_storage_encryption_key().expect("bootstrap should skip"),
            LocalAuthStorageKeyBootstrap::NotRequired
        );
        assert!(
            std::env::var("AUTH_STORAGE_ENCRYPTION_KEY").is_err(),
            "bootstrap should not set a key without durable auth storage"
        );
    }

    #[test]
    fn local_auth_storage_key_bootstrap_generates_and_reuses_key_file() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "AUTH_STORAGE_ENCRYPTION_KEY",
            "PLASM_SECRETS_DIR",
            "PLASM_AUTH_STORAGE_URL",
            "DATABASE_URL",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ]);
        clear_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PLASM_LOCAL_STATE_DIR", temp.path());
        std::env::set_var("DATABASE_URL", "postgresql://localhost/plasm");

        let first = ensure_local_auth_storage_encryption_key().expect("generate key");
        let first_path = match first {
            LocalAuthStorageKeyBootstrap::GeneratedFile { path } => path,
            other => panic!("expected generated file, got {other:?}"),
        };
        let first_key = std::env::var("AUTH_STORAGE_ENCRYPTION_KEY").expect("key set");
        assert_eq!(
            read_local_auth_storage_key(&first_path).expect("read generated key"),
            first_key
        );
        validate_auth_storage_encryption_key().expect("generated key validates");

        std::env::remove_var("AUTH_STORAGE_ENCRYPTION_KEY");
        let second = ensure_local_auth_storage_encryption_key().expect("reuse key");
        let second_path = match second {
            LocalAuthStorageKeyBootstrap::LoadedFromFile { path } => path,
            other => panic!("expected loaded key, got {other:?}"),
        };
        assert_eq!(second_path, first_path);
        assert_eq!(
            std::env::var("AUTH_STORAGE_ENCRYPTION_KEY").expect("reloaded key"),
            first_key
        );
    }

    #[test]
    fn local_auth_storage_key_bootstrap_respects_explicit_env() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "AUTH_STORAGE_ENCRYPTION_KEY",
            "PLASM_SECRETS_DIR",
            "PLASM_AUTH_STORAGE_URL",
            "DATABASE_URL",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ]);
        clear_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PLASM_LOCAL_STATE_DIR", temp.path());
        std::env::set_var("DATABASE_URL", "postgresql://localhost/plasm");
        let explicit_key = generate_auth_storage_encryption_key();
        std::env::set_var("AUTH_STORAGE_ENCRYPTION_KEY", &explicit_key);

        assert_eq!(
            ensure_local_auth_storage_encryption_key().expect("env should win"),
            LocalAuthStorageKeyBootstrap::ProvidedByEnv
        );
        assert_eq!(
            std::env::var("AUTH_STORAGE_ENCRYPTION_KEY").expect("env key remains"),
            explicit_key
        );
        assert!(
            !temp
                .path()
                .join(LOCAL_AUTH_STORAGE_KEY_RELATIVE_PATH)
                .exists(),
            "explicit env should avoid writing a local key file"
        );
    }

    #[test]
    fn local_appliance_jwt_secret_bootstrap_persists_when_unset() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "PLASM_AUTH_JWT_SECRET",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ]);
        clear_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PLASM_LOCAL_STATE_DIR", temp.path());

        let state = ensure_local_appliance_jwt_signing_secret().expect("jwt bootstrap");
        assert!(matches!(
            state,
            LocalAuthJwtSecretBootstrap::GeneratedFile { .. }
        ));
        let path = temp.path().join(LOCAL_AUTH_JWT_SECRET_RELATIVE_PATH);
        assert!(path.exists(), "jwt secret file should be written");
        let from_env = std::env::var("PLASM_AUTH_JWT_SECRET").expect("jwt env");
        assert!(!from_env.trim().is_empty());
    }

    #[test]
    fn local_auth_storage_key_bootstrap_reports_corrupt_key_file() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = EnvGuard::new(&[
            "AUTH_STORAGE_ENCRYPTION_KEY",
            "PLASM_SECRETS_DIR",
            "PLASM_AUTH_STORAGE_URL",
            "DATABASE_URL",
            "PLASM_LOCAL_STATE_DIR",
            "KUBERNETES_SERVICE_HOST",
        ]);
        clear_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PLASM_LOCAL_STATE_DIR", temp.path());
        std::env::set_var("DATABASE_URL", "postgresql://localhost/plasm");
        let path = temp.path().join(LOCAL_AUTH_STORAGE_KEY_RELATIVE_PATH);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdirs");
        std::fs::write(&path, "not-base64").expect("write corrupt key");

        let err =
            ensure_local_auth_storage_encryption_key().expect_err("corrupt key should be rejected");
        assert!(
            err.contains(&path.display().to_string()),
            "error should name the corrupt file path: {err}"
        );
        assert!(
            err.contains("orphan previously encrypted OAuth secrets and MCP API keys"),
            "error should explain the replacement consequence: {err}"
        );
    }
}
