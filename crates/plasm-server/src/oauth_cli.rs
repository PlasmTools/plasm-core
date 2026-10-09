//! `plasm-server oauth …` — provider registry + RFC 8628 device binding (in-process; mirrors HTTP/TUI).

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use clap::Subcommand;
use plasm_agent_core::mcp_config_repository::McpConfigRepository;
use plasm_agent_core::oauth_link_catalog::OauthLinkCatalog;

use crate::appliance_oauth_admin::{
    appliance_oauth_upsert_provider, AdminError, ApplianceOauthUpsert,
};

#[derive(Debug, clap::Args)]
pub struct OauthCliRoot {
    #[command(subcommand)]
    pub command: OauthCmd,
}

#[derive(Debug, Subcommand)]
pub enum OauthCmd {
    #[command(subcommand)]
    Provider(OauthProviderCmd),
    #[command(subcommand)]
    Device(OauthDeviceCmd),
}

#[derive(Debug, Subcommand)]
pub enum OauthProviderCmd {
    /// Rows from `oauth_provider_apps` when Postgres is configured (`DATABASE_URL` / embedded).
    List {
        #[arg(long)]
        json: bool,
    },
    Upsert {
        #[arg(long)]
        entry_id: String,
        #[arg(long)]
        token_endpoint: String,
        #[arg(long)]
        authorization_endpoint: Option<String>,
        #[arg(long)]
        device_authorization_endpoint: Option<String>,
        #[arg(long)]
        client_id: String,
        /// OAuth client secret stored for this provider (KV layout is fixed per `entry_id`).
        #[arg(long)]
        client_secret: Option<String>,
        /// Read client secret from stdin (used when `--client-secret` is omitted).
        #[arg(long)]
        client_secret_stdin: bool,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        /// When set, disables this provider in Postgres (or removes runtime-only catalog row).
        #[arg(long)]
        disabled: bool,
    },
    Disable {
        #[arg(long)]
        entry_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum OauthDeviceCmd {
    /// Begin device authorization; prints `user_code` and `verification_uri`.
    Start {
        #[arg(long)]
        entry_id: String,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Poll until authorized or timeout (calls token endpoint with `device_code`).
    Poll {
        #[arg(long)]
        entry_id: String,
        #[arg(long)]
        device_code: String,
        #[arg(long, default_value_t = 600)]
        max_wait_secs: u64,
        #[arg(long)]
        json: bool,
    },
}

async fn standalone_oauth_context() -> Result<
    (
        Option<Arc<McpConfigRepository>>,
        Arc<OauthLinkCatalog>,
        Arc<dyn plasm_agent_core::secret_store::SecretStore>,
    ),
    AdminError,
> {
    let storage = plasm_agent_core::secret_store_host::init_standalone_auth_storage()
        .await
        .map_err(|source| AdminError::AuthInitialization {
            source: Box::new(source),
        })?;
    let catalog = Arc::new(OauthLinkCatalog::from_env());
    let repo = match plasm_agent_core::mcp_config_repository::mcp_config_database_url() {
        Some(url) => Some(Arc::new(
            McpConfigRepository::connect_and_migrate(&url)
                .await
                .map_err(|source| AdminError::RepositoryInitialization { source })?,
        )),
        None => None,
    };
    Ok((repo, catalog, storage))
}

fn read_secret_stdin() -> Result<String, AdminError> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|source| AdminError::SecretInput { source })?;
    Ok(buf.trim().to_string())
}

pub async fn run_oauth(cli: OauthCliRoot) -> Result<(), AdminError> {
    match cli.command {
        OauthCmd::Provider(cmd) => run_oauth_provider(cmd).await,
        OauthCmd::Device(cmd) => run_oauth_device(cmd).await,
    }
}

async fn run_oauth_provider(cmd: OauthProviderCmd) -> Result<(), AdminError> {
    let (repo, catalog, storage) = standalone_oauth_context().await?;

    match cmd {
        OauthProviderCmd::List { json } => {
            let Some(ref r) = repo else {
                eprintln!(
                    "Could not open the appliance database (same as when you run the control station)."
                );
                return Ok(());
            };
            let rows =
                plasm_agent_core::oauth_provider_repository::list_oauth_provider_apps(r.pool())
                    .await
                    .map_err(|source| AdminError::ProviderDatabase {
                        source: Box::new(source.into()),
                    })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows)
                        .map_err(|source| AdminError::ProviderListSerialization { source })?
                );
            } else {
                for row in rows {
                    println!(
                        "{}\tenabled={}\tauth={:?}\tdevice={:?}\ttoken={:?}",
                        row.entry_id,
                        row.enabled,
                        row.authorization_endpoint.as_deref().unwrap_or(""),
                        row.device_authorization_endpoint.as_deref().unwrap_or(""),
                        row.token_endpoint.as_deref().unwrap_or(""),
                    );
                }
            }
        }
        OauthProviderCmd::Upsert {
            entry_id,
            token_endpoint,
            authorization_endpoint,
            device_authorization_endpoint,
            client_id,
            client_secret,
            client_secret_stdin,
            scope,
            disabled,
        } => {
            let enabled = !disabled;
            let secret_val = if client_secret_stdin {
                Some(read_secret_stdin()?)
            } else {
                client_secret
            };
            let client_secret_key =
                crate::appliance_oauth_admin::appliance_oauth_client_secret_kv_key(&entry_id)?;
            appliance_oauth_upsert_provider(
                repo.as_deref(),
                catalog.as_ref(),
                &storage,
                ApplianceOauthUpsert {
                    entry_id,
                    authorization_endpoint,
                    token_endpoint,
                    device_authorization_endpoint,
                    default_scopes: scope,
                    client_id,
                    client_secret_key,
                    client_secret_value: secret_val,
                    enabled,
                },
            )
            .await?;
            println!("ok");
        }
        OauthProviderCmd::Disable { entry_id } => {
            crate::appliance_oauth_admin::appliance_oauth_provider_disable(
                repo.as_deref(),
                catalog.as_ref(),
                &entry_id,
            )
            .await?;
            println!("ok");
        }
    }
    Ok(())
}

async fn run_oauth_device(cmd: OauthDeviceCmd) -> Result<(), AdminError> {
    let (_repo, catalog, storage) = standalone_oauth_context().await?;

    match cmd {
        OauthDeviceCmd::Start {
            entry_id,
            scope,
            json,
        } => {
            let cfg = catalog
                .resolve_for_oauth_start(&storage, entry_id.trim())
                .await
                .map_err(|source| AdminError::ProviderResolution {
                    source: Box::new(source),
                })?;
            let device_url = cfg
                .device_authorization_endpoint
                .as_deref()
                .map(str::trim)
                .filter(|s: &&str| !s.is_empty())
                .ok_or_else(|| AdminError::MissingDeviceEndpoint {
                    entry_id: entry_id.clone(),
                })?;
            let http = plasm_runtime::build_oauth_token_http_client(Duration::from_secs(30))
                .map_err(|source| AdminError::HttpClient { source })?;
            let resp = plasm_runtime::request_oauth_device_authorization(
                &http,
                device_url,
                cfg.client_id.trim(),
                Some(cfg.client_secret.as_str()),
                &scope,
                Duration::from_secs(30),
            )
            .await
            .map_err(|source| AdminError::DeviceAuthorization { source })?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "device_code": resp.device_code,
                        "user_code": resp.user_code,
                        "verification_uri": resp.verification_uri,
                        "verification_uri_complete": resp.verification_uri_complete,
                        "expires_in": resp.expires_in,
                        "interval": resp.interval.unwrap_or(5),
                    })
                );
            } else {
                println!("user_code: {}", resp.user_code);
                println!("verification_uri: {}", resp.verification_uri);
                if let Some(ref u) = resp.verification_uri_complete {
                    println!("verification_uri_complete: {u}");
                }
                println!("device_code: {}", resp.device_code);
                println!("expires_in: {}", resp.expires_in);
                println!("interval: {}", resp.interval.unwrap_or(5));
            }
        }
        OauthDeviceCmd::Poll {
            entry_id,
            device_code,
            max_wait_secs,
            json,
        } => {
            let cfg = catalog
                .resolve_for_oauth_start(&storage, entry_id.trim())
                .await
                .map_err(|source| AdminError::ProviderResolution {
                    source: Box::new(source),
                })?;
            let http = plasm_runtime::build_oauth_token_http_client(Duration::from_secs(30))
                .map_err(|source| AdminError::HttpClient { source })?;
            let http_timeout = Duration::from_secs(30);
            let mut interval = Duration::from_secs(5);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(max_wait_secs.max(1));

            loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(AdminError::DeviceTimedOut {
                        wait_secs: max_wait_secs.max(1),
                    });
                }
                match plasm_runtime::poll_oauth_device_token_once(
                    &http,
                    cfg.token_endpoint.trim(),
                    cfg.client_id.trim(),
                    Some(cfg.client_secret.as_str()),
                    device_code.trim(),
                    http_timeout,
                )
                .await
                .map_err(|source| AdminError::DevicePoll { source })?
                {
                    plasm_runtime::OAuthDeviceTokenPoll::Success(token_json) => {
                        let envelope = plasm_runtime::OutboundOAuthKvV1::from_token_json_for_entry(
                            entry_id.trim().to_string(),
                            &token_json,
                        )
                        .map_err(|source| AdminError::TokenEnvelope { source })?;
                        let hosted_kv_key = format!("plasm:outbound:v1:{}", uuid::Uuid::new_v4());
                        let envelope_bytes = serde_json::to_vec(&envelope)
                            .map_err(|source| AdminError::TokenSerialization { source })?;
                        storage
                            .store_kv(&hosted_kv_key, &envelope_bytes, None)
                            .await
                            .map_err(|source| AdminError::TokenWrite {
                                key: hosted_kv_key.clone(),
                                source: Box::new(source),
                            })?;
                        plasm_agent_core::oauth_binding_kv::write_oauth_binding_pointer(
                            &storage,
                            entry_id.trim(),
                            &hosted_kv_key,
                        )
                        .await
                        .map_err(|source| {
                            AdminError::BindingPointerWrite {
                                entry_id: entry_id.trim().to_owned(),
                                source: Box::new(source),
                            }
                        })?;
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "poll_status": "completed",
                                    "hosted_kv_key": hosted_kv_key,
                                    "entry_id": entry_id.trim(),
                                })
                            );
                        } else {
                            println!("poll_status: completed");
                            println!("hosted_kv_key: {hosted_kv_key}");
                        }
                        break;
                    }
                    plasm_runtime::OAuthDeviceTokenPoll::AuthorizationPending => {
                        tokio::time::sleep(interval).await;
                    }
                    plasm_runtime::OAuthDeviceTokenPoll::SlowDown { interval_secs } => {
                        interval = Duration::from_secs(interval_secs.max(1));
                        tokio::time::sleep(interval).await;
                    }
                    plasm_runtime::OAuthDeviceTokenPoll::OAuthError {
                        error,
                        error_description,
                    } => {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "poll_status": "error",
                                    "error": error,
                                    "error_description": error_description,
                                })
                            );
                        } else {
                            eprintln!("poll_status: error");
                            eprintln!("error: {error}");
                            if let Some(d) = error_description {
                                eprintln!("error_description: {d}");
                            }
                        }
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod appliance_oauth_client_secret_kv_key_tests {
    use crate::appliance_oauth_admin::appliance_oauth_client_secret_kv_key;

    #[test]
    fn key_is_entry_scoped() {
        assert_eq!(
            appliance_oauth_client_secret_kv_key("github").unwrap(),
            "plasm:oauth_app:v1:github"
        );
    }

    #[test]
    fn empty_entry_id_errors() {
        assert!(appliance_oauth_client_secret_kv_key("  ").is_err());
    }
}
