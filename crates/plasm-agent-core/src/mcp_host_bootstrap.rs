//! Shared catalog/engine/registry bootstrap for `plasm-mcp` (OSS), hosted `plasm-mcp-saas`, and
//! `plasm-server`. Keeps a single code path for loading CGS, validating templates, building the
//! registry snapshot, and attaching OSS-side extensions (outbound OAuth KV, MCP policy sqlx,
//! discovery embeddings).

use std::path::Path;
use std::sync::Arc;

use clap::{Arg, ArgAction, ArgMatches, Command};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::schema::CGS;
use plasm_core::{PromptPipelineConfig, PromptRenderMode};
use plasm_plugin_host::PluginManager;
use plasm_runtime::{
    AuthResolver, ExecutionConfig, ExecutionEngine, ExecutionMode, SecretProvider,
};

use crate::catalog_runtime::CatalogBootstrap;
use crate::error::AgentError;
use crate::http::PlasmHostBootstrap;
use crate::incoming_auth::{IncomingAuthConfig, IncomingAuthVerifier};
use crate::mcp_config_repository::{self, McpConfigRepositoryError};
use crate::run_artifacts::RunArtifactInitPolicy;
use crate::server_state::PlasmHostState;

/// Early argv parse shared by `plasm-mcp`, `plasm-mcp-saas`, and `plasm-server`.
/// Keep in sync with Helm `deploy/charts/plasm-mcp/values.yaml` default args.
pub fn preparse_mcp_command() -> Command {
    Command::new("plasm-mcp")
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
            Arg::new("listen_host")
                .long("listen-host")
                .value_name("HOST")
                .num_args(1)
                .required(false),
        )
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
        .ignore_errors(true)
}

/// Result of the early `--schema` / `--plugin-dir` parse before the full CLI match.
pub struct CatalogLoadOutcome {
    pub schema_path: String,
    pub cgs: CGS,
    /// When `--plugin-dir` was used, the registry snapshot (shared Arc — no second disk load).
    pub prebuilt_registry: Option<Arc<InMemoryCgsRegistry>>,
}

/// Load CGS + optional preloaded multi-entry registry from `pre_matches` (same rules as `plasm-mcp`).
pub fn load_catalog_for_mcp_server(
    pre_matches: &ArgMatches,
    server_mode: bool,
) -> Result<CatalogLoadOutcome, AgentError> {
    load_catalog_for_mcp_server_with_progress(pre_matches, server_mode, &mut |_: &str| {})
}

/// Same as [`load_catalog_for_mcp_server`] with progress callbacks (e.g. appliance BOOT Detail).
pub fn load_catalog_for_mcp_server_with_progress<P: FnMut(&str)>(
    pre_matches: &ArgMatches,
    server_mode: bool,
    progress: &mut P,
) -> Result<CatalogLoadOutcome, AgentError> {
    let plugin_dir = pre_matches.get_one::<String>("plugin_dir");
    match pre_matches.get_one::<String>("schema") {
        Some(path) => {
            if plugin_dir.is_some() {
                return Err(AgentError::Schema(
                    "do not combine --schema with --plugin-dir".into(),
                ));
            }
            progress(&format!("loading CGS schema from {path}"));
            let cgs =
                plasm_core::loader::load_schema(Path::new(path)).map_err(AgentError::Schema)?;
            progress("loaded single-schema catalog");
            Ok(CatalogLoadOutcome {
                schema_path: path.clone(),
                cgs,
                prebuilt_registry: None,
            })
        }
        None => {
            if server_mode {
                if let Some(pd) = plugin_dir {
                    progress("loading multi-entry registry from plugin-dir…");
                    let reg = crate::plugin_catalog::load_registry_from_plugin_dir_with_progress(
                        Path::new(pd),
                        progress,
                    )
                    .map_err(AgentError::Schema)?;
                    let reg = Arc::new(reg);
                    let arc_cgs = reg.first_cgs().ok_or_else(|| {
                        AgentError::Schema("plugin-dir catalog has no entries".into())
                    })?;
                    let cgs = (*arc_cgs).clone();
                    Ok(CatalogLoadOutcome {
                        schema_path: pd.clone(),
                        cgs,
                        prebuilt_registry: Some(reg),
                    })
                } else {
                    Err(AgentError::Schema(
                        "pass --schema <path> or --plugin-dir <dir> with --http/--mcp".into(),
                    ))
                }
            } else {
                Err(AgentError::Schema(
                    "pass --schema <path> for non-server modes".into(),
                ))
            }
        }
    }
}

pub fn validate_catalog_templates(outcome: &CatalogLoadOutcome) -> Result<(), AgentError> {
    validate_catalog_templates_with_progress(outcome, &mut |_: &str| {})
}

pub fn validate_catalog_templates_with_progress<P: FnMut(&str)>(
    outcome: &CatalogLoadOutcome,
    progress: &mut P,
) -> Result<(), AgentError> {
    if let Some(reg) = &outcome.prebuilt_registry {
        crate::plugin_catalog::validate_registry_templates_with_progress(reg, progress)
            .map_err(AgentError::Schema)?;
    } else {
        progress("validating capability templates (single schema)…");
        plasm_compile::validate_cgs_capability_templates(&outcome.cgs)
            .map_err(|e| AgentError::Schema(e.to_string()))?;
    }
    Ok(())
}

pub fn build_registry_arc(
    matches: &ArgMatches,
    outcome: &CatalogLoadOutcome,
) -> Result<Arc<InMemoryCgsRegistry>, AgentError> {
    if let Some(reg) = &outcome.prebuilt_registry {
        return Ok(Arc::clone(reg));
    }
    if let Some(pd) = matches.get_one::<String>("plugin_dir") {
        let reg = crate::plugin_catalog::load_registry_from_plugin_dir(Path::new(pd))
            .map_err(AgentError::Schema)?;
        return Ok(Arc::new(reg));
    }
    Ok(Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        "default".into(),
        "default".into(),
        vec![],
        Arc::new(outcome.cgs.clone()),
    )])))
}

pub fn catalog_bootstrap_from_matches(matches: &ArgMatches) -> CatalogBootstrap {
    if matches.get_one::<String>("plugin_dir").is_some() {
        CatalogBootstrap::PluginDir {
            path: std::path::PathBuf::from(
                matches
                    .get_one::<String>("plugin_dir")
                    .expect("plugin-dir checked"),
            ),
        }
    } else {
        CatalogBootstrap::Fixed
    }
}

pub fn build_execution_engine_from_matches(
    matches: &ArgMatches,
    schema_path: &str,
    cgs: &CGS,
) -> Result<(ExecutionEngine, ExecutionMode), AgentError> {
    let backend_raw = matches
        .get_one::<String>("backend")
        .map(|s| s.as_str())
        .unwrap_or("http://localhost:1080");
    let backend = crate::backend_normalize::normalize_live_backend_url(schema_path, backend_raw);
    crate::http_backend::ReplHttpOverride::from_cli_normalized(backend.as_ref())
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

    let prompt_focus = matches.get_one::<String>("focus").cloned();
    let render_mode = matches
        .get_one::<String>("symbol_tuning")
        .map(|s| PromptRenderMode::parse_user_facing_or_default(s))
        .unwrap_or_default();
    let prompt_pipeline =
        PromptPipelineConfig::for_cli_focus(prompt_focus.as_deref()).with_render_mode(render_mode);

    let mut config = ExecutionConfig {
        base_url: Some(backend.to_string()),
        prompt_pipeline,
        ..ExecutionConfig::default()
    };
    config.apply_http_env_overrides();

    let auth_resolver = cgs.auth.clone().map(AuthResolver::from_env);
    let engine = ExecutionEngine::new_with_auth(config, auth_resolver)?;
    Ok((engine, mode))
}

pub fn plugin_manager_from_matches(
    matches: &ArgMatches,
) -> Result<Option<Arc<PluginManager>>, std::io::Error> {
    match matches.get_one::<String>("compile_plugin") {
        Some(path) => {
            let pm = PluginManager::load(Path::new(path))
                .map_err(|e| std::io::Error::other(format!("--compile-plugin {path}: {e}")))?;
            Ok(Some(Arc::new(pm)))
        }
        None => Ok(None),
    }
}

/// Validates incoming-auth env and returns a verifier for HTTP/MCP middleware.
pub fn incoming_verifier_from_env() -> Result<Arc<IncomingAuthVerifier>, std::io::Error> {
    let incoming_cfg = IncomingAuthConfig::from_env();
    incoming_cfg
        .validate_startup()
        .map_err(std::io::Error::other)?;
    let v =
        Arc::new(IncomingAuthVerifier::new(incoming_cfg.clone()).map_err(std::io::Error::other)?);
    crate::incoming_auth::log_incoming_auth_startup(&incoming_cfg, &v);
    Ok(v)
}

/// Inputs for [`build_initial_host_state`].
pub struct BuildInitialHostStateArgs {
    pub engine: ExecutionEngine,
    pub mode: ExecutionMode,
    pub registry: Arc<InMemoryCgsRegistry>,
    pub catalog_bootstrap: CatalogBootstrap,
    pub plugin_manager: Option<Arc<PluginManager>>,
    pub incoming_auth: Option<Arc<IncomingAuthVerifier>>,
    pub run_artifacts_policy: RunArtifactInitPolicy,
    pub oss_local_filesystem_defaults: bool,
}

/// Initial [`PlasmHostState`] after engine + incoming auth + artifact/session stores.
pub async fn build_initial_host_state(
    BuildInitialHostStateArgs {
        engine,
        mode,
        registry,
        catalog_bootstrap,
        plugin_manager,
        incoming_auth,
        run_artifacts_policy,
        oss_local_filesystem_defaults,
    }: BuildInitialHostStateArgs,
) -> Result<PlasmHostState, std::io::Error> {
    let run_artifacts = crate::run_artifacts::init_from_env_with_policy(run_artifacts_policy)
        .map_err(|e| std::io::Error::other(format!("run artifacts: {e}")))?;
    let session_graph_persistence = crate::session_graph_persistence::init_from_env()
        .map_err(|e| std::io::Error::other(format!("session graph persistence: {e}")))?;

    Ok(crate::http::build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode,
        registry,
        catalog_bootstrap,
        plugin_manager,
        incoming_auth,
        run_artifacts,
        session_graph_persistence,
        oss_local_filesystem_defaults,
    }))
}

fn outbound_oauth_enabled_from_env() -> bool {
    std::env::var("PLASM_OUTBOUND_OAUTH")
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
            .is_some()
}

/// OSS-only: optional standalone auth KV + outbound secret provider (same logic as `plasm-mcp` OSS).
pub async fn attach_outbound_oauth_if_enabled_oss(state: &mut PlasmHostState) {
    if !outbound_oauth_enabled_from_env() {
        return;
    }
    match crate::auth_framework_host::init_standalone_auth_storage().await {
        Ok(storage) => {
            let catalog = Arc::new(crate::oauth_link_catalog::OauthLinkCatalog::from_env());
            let outbound = Arc::new(
                crate::outbound_secret_provider::AgentOutboundSecretProvider::new(
                    storage.clone(),
                    catalog.clone(),
                ),
            );
            state.oss.auth_storage = Some(storage);
            state.oss.oauth_link_catalog = Some(catalog);
            state.oss.outbound_secret_provider = Some(outbound as Arc<dyn SecretProvider>);
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "outbound OAuth: auth storage init failed; set PLASM_AUTH_STORAGE_URL or DATABASE_URL, or PLASM_OUTBOUND_OAUTH=1 with a valid storage configuration"
            );
        }
    }
}

/// Result of attaching the OSS `project_mcp_*` policy store during host bootstrap.
#[derive(Debug)]
pub enum McpPolicyAttachOutcome {
    Attached,
    NoDatabaseUrl,
    Failed(McpConfigRepositoryError),
}

/// Full OSS host assembly after CLI matches + catalog outcome (no listeners).
pub struct OssHostBootstrap {
    pub state: PlasmHostState,
    pub mcp_policy_attach: McpPolicyAttachOutcome,
}

/// Ensure encrypted auth KV, [`auth_framework::AuthFramework`], and MCP API-key registry (idempotent).
pub async fn ensure_auth_framework_on_host(
    state: &mut PlasmHostState,
) -> Result<(), auth_framework::AuthError> {
    if state.auth_framework().is_some() {
        return Ok(());
    }
    let (storage, framework, mcp_api_keys) = match state.oss.auth_storage.clone() {
        Some(existing) => {
            let framework =
                crate::auth_framework_host::init_auth_framework_on_storage(existing.clone())
                    .await?;
            let mcp_api_keys = Arc::new(crate::mcp_api_key_registry::McpApiKeyRegistry::new(
                existing.clone(),
            ));
            (existing, framework, mcp_api_keys)
        }
        None => crate::auth_framework_host::init_standalone_auth_bundle().await?,
    };
    state.oss.auth_storage = Some(storage);
    attach_auth_framework_to_host(state, framework, mcp_api_keys);
    Ok(())
}

/// OSS-only: `project_mcp_*` + MCP API keys when a config DB URL resolves.
pub async fn attach_oss_mcp_policy_store(state: &mut PlasmHostState) -> McpPolicyAttachOutcome {
    let Some(db_url) = mcp_config_repository::mcp_config_database_url() else {
        return McpPolicyAttachOutcome::NoDatabaseUrl;
    };
    let repo = match mcp_config_repository::McpConfigRepository::connect_and_migrate(&db_url).await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "OSS plasm-mcp: project_mcp_* connect/migrate failed"
            );
            return McpPolicyAttachOutcome::Failed(e);
        }
    };

    if let Err(e) = ensure_auth_framework_on_host(state).await {
        tracing::warn!(
            error = %e,
            "OSS plasm-mcp: auth-framework init failed; MCP policy attached but /v1/auth/status will return 503"
        );
    }

    match &mut state.saas {
        Some(saas) => {
            saas.mcp_config_repository = Some(Arc::new(repo));
        }
        None => {
            state.saas = Some(crate::server_state::PlasmSaaSHostExtension {
                auth_framework: None,
                mcp_config_repository: Some(Arc::new(repo)),
                mcp_transport_auth: Some(
                    crate::auth_framework_host::mcp_api_key_registry_memory_only(),
                ),
                tenant_binding: None,
            });
        }
    };
    tracing::info!(
        "OSS plasm-mcp: tenant MCP policy enabled (project_mcp_* + API keys); control-plane routes on HTTP require X-Plasm-Control-Plane-Secret"
    );
    McpPolicyAttachOutcome::Attached
}

/// Wire [`AuthFramework`] (and refresh MCP API-key registry) on an existing host.
pub fn attach_auth_framework_to_host(
    state: &mut PlasmHostState,
    framework: Arc<tokio::sync::Mutex<auth_framework::AuthFramework>>,
    mcp_api_keys: Arc<crate::mcp_api_key_registry::McpApiKeyRegistry>,
) {
    match &mut state.saas {
        Some(saas) => {
            saas.auth_framework = Some(framework);
            saas.mcp_transport_auth = Some(mcp_api_keys);
        }
        None => {
            state.saas = Some(crate::server_state::PlasmSaaSHostExtension {
                auth_framework: Some(framework),
                mcp_config_repository: None,
                mcp_transport_auth: Some(mcp_api_keys),
                tenant_binding: None,
            });
        }
    }
}

/// Background reconcile for typed-discovery embeddings when Postgres store is configured.
pub async fn attach_discovery_embedding_background(mut state: PlasmHostState) {
    if let Some(repo) =
        crate::discovery_embedding_repository::maybe_connect_discovery_embedding_store().await
    {
        state.oss.discovery_embedding = Some(repo.clone());
        #[cfg(feature = "local-embeddings")]
        crate::discovery_embedding_reconcile::spawn_discovery_embedding_reconcile_background(
            state, repo,
        );
        #[cfg(not(feature = "local-embeddings"))]
        tracing::info!(
            "discovery embedding store configured but `local-embeddings` feature disabled; \
             skipping background reconcile (lexical-only discovery)"
        );
    }
}

/// Full OSS `plasm-mcp` host assembly after CLI matches + catalog outcome (no listeners).
pub async fn bootstrap_plasm_host_state_oss(
    matches: &ArgMatches,
    catalog_outcome: &CatalogLoadOutcome,
) -> Result<OssHostBootstrap, std::io::Error> {
    let registry = build_registry_arc(matches, catalog_outcome)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let catalog_bootstrap = catalog_bootstrap_from_matches(matches);
    let (engine, mode) = build_execution_engine_from_matches(
        matches,
        catalog_outcome.schema_path.as_str(),
        &catalog_outcome.cgs,
    )
    .map_err(|e| std::io::Error::other(e.to_string()))?;
    let plugin_manager = plugin_manager_from_matches(matches)?;
    let incoming_verifier = incoming_verifier_from_env()?;
    let mut app_state = build_initial_host_state(BuildInitialHostStateArgs {
        engine,
        mode,
        registry,
        catalog_bootstrap,
        plugin_manager,
        incoming_auth: Some(incoming_verifier),
        run_artifacts_policy: crate::run_artifacts::RunArtifactInitPolicy::OssFilesystemDefaults,
        oss_local_filesystem_defaults: true,
    })
    .await?;
    attach_outbound_oauth_if_enabled_oss(&mut app_state).await;
    let mcp_policy_attach = attach_oss_mcp_policy_store(&mut app_state).await;
    if let Err(e) = ensure_auth_framework_on_host(&mut app_state).await {
        tracing::warn!(
            error = %e,
            "OSS host bootstrap: auth-framework init failed; /v1/auth/status will return 503"
        );
    }
    attach_discovery_embedding_background(app_state.clone()).await;
    Ok(OssHostBootstrap {
        state: app_state,
        mcp_policy_attach,
    })
}
