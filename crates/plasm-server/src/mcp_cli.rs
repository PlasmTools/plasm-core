//! Non-interactive `plasm-server mcp …` commands.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Subcommand;
use plasm_agent_core::error::AgentError;
use plasm_agent_core::mcp_config_admin::McpConfigAdminService;
use plasm_agent_core::mcp_host_bootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use uuid::Uuid;

use crate::appliance_mcp_admin::{appliance_mcp_scope, appliance_preferred_config_id};

#[derive(Debug, clap::Args)]
pub struct McpCliRoot {
    /// CGS schema path (optional metadata for `apis list` labels / auth markers).
    #[arg(long, value_name = "PATH", group = "mcp_catalog")]
    pub schema: Option<PathBuf>,
    /// Packed plugin directory (optional metadata for `apis list`).
    #[arg(long, value_name = "DIR", group = "mcp_catalog")]
    pub catalog_dir: Option<PathBuf>,
    #[arg(long)]
    pub symbol_tuning: Option<String>,
    #[command(subcommand)]
    pub command: McpCmd,
}

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// Show resolved singleton config, listener-oriented hints, and counts (`--json` for machine output).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Ensure the singleton MCP config row exists for the appliance synthetic tenant.
    Init {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(subcommand)]
    Apis(ApisCmd),
    #[command(subcommand)]
    Keys(KeysCmd),
    /// Run `project_mcp_*` sqlx migrations then exit (same rules as legacy `--migrate-mcp-config-db`).
    ///
    /// Requires reachable Postgres (`DATABASE_URL` / auth storage URL). Embedded Postgres autostart fills these when applicable.
    MigrateDb,
}

#[derive(Debug, Subcommand)]
pub enum ApisCmd {
    /// List catalog/API rows (`--enabled` restricts to MCP-enabled entry ids).
    List {
        #[arg(long)]
        enabled: bool,
        #[arg(long)]
        json: bool,
    },
    Enable {
        #[arg(required = true)]
        entry_ids: Vec<String>,
    },
    Disable {
        #[arg(required = true)]
        entry_ids: Vec<String>,
    },
    /// Replace the enabled API set exactly (allowlist = given ids).
    Set {
        #[arg(required = true)]
        entry_ids: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum KeysCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        json: bool,
    },
    Reveal {
        key_id: Uuid,
    },
    Rotate {
        key_id: Uuid,
        #[arg(long)]
        name: String,
    },
    Revoke {
        key_id: Uuid,
    },
}

pub async fn run_mcp(cli: McpCliRoot) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if matches!(&cli.command, McpCmd::MigrateDb) {
        return crate::run_migrate_mcp_config_db().await;
    }

    let registry = load_optional_registry(&cli).await?;

    let svc = crate::appliance_mcp_admin::connect_standalone_mcp_admin_service().await?;
    let scope = appliance_mcp_scope();
    let pref = appliance_preferred_config_id();

    match cli.command {
        McpCmd::MigrateDb => unreachable!(),
        McpCmd::Status { json } => {
            let id = match svc.resolve_existing_config_id(&scope, pref).await {
                Ok(Some(id)) => id,
                Ok(None) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "policy_store_configured": true,
                                "singleton_config_present": false,
                                "scope": scope_json(&scope),
                            })
                        );
                    } else {
                        println!("MCP policy store: connected");
                        println!("Singleton config: (none — run `plasm-server mcp init`)");
                    }
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            let summary = svc.admin_summary(id).await?;
            let runtime = svc
                .load_runtime_snapshot(id)
                .await?
                .ok_or_else(|| format!("runtime snapshot missing for config {id}"))?;
            let optional = svc.load_auth_optional_set(id).await?;
            let rows = if let Some(reg) = registry.as_ref() {
                McpConfigAdminService::catalog_rows(reg, &runtime, &optional)
            } else {
                // Labels/default markers without CGS metadata (entry ids still accurate).
                let empty = InMemoryCgsRegistry::from_pairs(vec![]);
                McpConfigAdminService::catalog_rows(&empty, &runtime, &optional)
            };
            let mut optv: Vec<String> = optional.iter().cloned().collect();
            optv.sort();
            let coverage = McpConfigAdminService::auth_coverage(&rows, &optv);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "policy_store_configured": true,
                        "singleton_config_present": true,
                        "scope": scope_json(&scope),
                        "summary": summary,
                        "auth_coverage": coverage,
                        "catalog_rows": rows,
                    })
                );
            } else {
                println!("config_id: {}", summary.config_id);
                println!("name: {}  status: {}", summary.name, summary.status);
                println!(
                    "enabled APIs: {}  transport keys: {}",
                    summary.enabled_api_count, summary.api_key_count
                );
                if !coverage.entries_missing_binding_when_required.is_empty() {
                    println!(
                        "missing outbound binding for: {}",
                        coverage.entries_missing_binding_when_required.join(", ")
                    );
                }
            }
            Ok(())
        }
        McpCmd::Init { name, json } => {
            let nm = name.unwrap_or_else(|| "Your MCP".to_string());
            let id = svc.ensure_singleton_config(&scope, pref, &nm).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "config_id": id.to_string(), "name": nm })
                );
            } else {
                println!("Ensured singleton MCP config {id} ({nm})");
            }
            Ok(())
        }
        McpCmd::Apis(sub) => {
            let id = svc
                .ensure_singleton_config(&scope, pref, "Your MCP")
                .await?;
            match sub {
                ApisCmd::List { enabled, json } => {
                    let runtime = svc
                        .load_runtime_snapshot(id)
                        .await?
                        .ok_or_else(|| format!("runtime snapshot missing for config {id}"))?;
                    let optional = svc.load_auth_optional_set(id).await?;
                    let empty_reg = InMemoryCgsRegistry::from_pairs(vec![]);
                    let reg_ref: &InMemoryCgsRegistry = match registry.as_ref() {
                        Some(a) => a.as_ref(),
                        None => &empty_reg,
                    };
                    let mut rows =
                        McpConfigAdminService::catalog_rows(reg_ref, &runtime, &optional);
                    if enabled {
                        rows.retain(|r| r.enabled_for_mcp);
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        for r in rows {
                            let on = if r.enabled_for_mcp { "[on]" } else { "[off]" };
                            println!("{} {} — {}  {:?}", on, r.entry_id, r.label, r.auth_marker);
                        }
                    }
                }
                ApisCmd::Enable { entry_ids } => {
                    for e in entry_ids {
                        svc.enable_api(id, &e).await?;
                    }
                }
                ApisCmd::Disable { entry_ids } => {
                    for e in entry_ids {
                        svc.disable_api(id, &e).await?;
                    }
                }
                ApisCmd::Set { entry_ids } => {
                    let set: std::collections::HashSet<String> = entry_ids
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    svc.set_allowed_apis_exact(id, set).await?;
                }
            }
            Ok(())
        }
        McpCmd::Keys(sub) => {
            let id = svc
                .ensure_singleton_config(&scope, pref, "Your MCP")
                .await?;
            match sub {
                KeysCmd::List { json } => {
                    let rows = svc.list_api_key_rows(id).await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        for r in rows {
                            println!("{}  fp:{}  {:?}", r.key_id, r.fingerprint, r.label);
                        }
                    }
                }
                KeysCmd::Add { name, json } => {
                    let out = svc.provision_api_key(id, name).await?;
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "key_id": out.key_id.to_string(),
                                "api_key": out.api_key,
                            })
                        );
                    } else {
                        println!("key_id={}", out.key_id);
                        println!("api_key={}", out.api_key);
                    }
                }
                KeysCmd::Reveal { key_id } => {
                    let raw = svc.reveal_api_key(id, key_id).await?;
                    println!("{raw}");
                }
                KeysCmd::Rotate { key_id, name } => {
                    let out = svc.rotate_one_api_key(id, key_id, name).await?;
                    println!("key_id={}\napi_key={}", out.key_id, out.api_key);
                }
                KeysCmd::Revoke { key_id } => {
                    svc.revoke_one_api_key(id, key_id).await?;
                }
            }
            Ok(())
        }
    }
}

fn scope_json(scope: &plasm_agent_core::mcp_config_admin::McpConfigScope) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": scope.tenant_id,
        "workspace_slug": scope.workspace_slug,
        "project_slug": scope.project_slug,
        "space_type": scope.space_type,
        "owner_subject": scope.owner_subject,
    })
}

async fn load_optional_registry(
    cli: &McpCliRoot,
) -> Result<Option<Arc<InMemoryCgsRegistry>>, Box<dyn std::error::Error + Send + Sync>> {
    match (&cli.schema, &cli.catalog_dir) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => {
            Err("pass at most one of --schema or --catalog-dir for mcp commands".into())
        }
        (schema_path, catalog_dir) => {
            let mut argv = vec![std::ffi::OsString::from("plasm-server-mcp-catalog")];
            if let Some(pd) = catalog_dir {
                argv.push(std::ffi::OsString::from("--catalog-dir"));
                argv.push(pd.clone().into_os_string());
            } else if let Some(sp) = schema_path {
                argv.push(std::ffi::OsString::from("--schema"));
                argv.push(sp.clone().into_os_string());
            }
            if let Some(ref st) = cli.symbol_tuning {
                argv.push(std::ffi::OsString::from("--symbol-tuning"));
                argv.push(std::ffi::OsString::from(st));
            }
            let reg = tokio::task::spawn_blocking(move || {
                let pre = mcp_host_bootstrap::preparse_mcp_command()
                    .try_get_matches_from(&argv)
                    .map_err(|e| AgentError::Argument(format!("mcp catalog argv: {e:#}")))?;
                let outcome = mcp_host_bootstrap::load_catalog_for_mcp_server(&pre, false)?;
                mcp_host_bootstrap::validate_catalog_templates(&outcome)?;
                mcp_host_bootstrap::build_registry_arc(&pre, &outcome)
            })
            .await??;
            Ok(Some(reg))
        }
    }
}
