//! Integration coverage for [`plasm_agent_core::mcp_config_admin`] against real Postgres when Docker is available.
//!
//! This is the **headless** contract for MCP API key provisioning (`McpConfigAdminService::provision_api_key`),
//! the same backend call the appliance TUI issues via `AdminJob::ProvisionApiKey` — it does **not**
//! exercise Ratatui/Crossterm or PTY redraw behavior.
//!
//! Override with `PLASM_TEST_POSTGRES_URL=postgres://...` to use an existing server.

mod support;

use std::sync::Arc;

use plasm_agent_core::mcp_config_admin::{McpConfigAdminService, McpConfigScope};
use plasm_agent_core::mcp_config_repository::McpConfigRepository;
use plasm_agent_core::secret_store_host;
use support::postgres::{integration_postgres_url, INTEGRATION_POSTGRES_URL_ENV};

#[tokio::test]
async fn singleton_allowlist_and_keys_roundtrip() {
    const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
    let Some((_postgres, db_url)) = integration_postgres_url(START_TIMEOUT).await else {
        eprintln!(
            "mcp_config_admin integration: skipping (no Docker / Postgres). \
             Set {INTEGRATION_POSTGRES_URL_ENV} or ensure Docker is running."
        );
        return;
    };
    let repo = Arc::new(
        McpConfigRepository::connect_and_migrate(&db_url)
            .await
            .expect("migrate"),
    );
    let keys = secret_store_host::memory_auth_bundle().0;
    let svc = McpConfigAdminService::new(repo, keys);

    let scope = McpConfigScope::organization_workspace_project(
        format!(
            "admintest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
        "default".into(),
        "default".into(),
    );

    let id = svc
        .ensure_singleton_config(&scope, None, "Your MCP")
        .await
        .expect("ensure");

    svc.set_allowed_apis_exact(id, ["alpha".into(), "beta".into()].into())
        .await
        .expect("set apis");

    let rt = svc
        .load_runtime_snapshot(id)
        .await
        .expect("snap")
        .expect("runtime");
    assert!(rt.allowed_entry_ids.contains("alpha"));
    assert!(rt.allowed_entry_ids.contains("beta"));

    let prov = svc
        .provision_api_key(id, "cli-test".into())
        .await
        .expect("provision");
    assert!(!prov.api_key.is_empty());

    let listed = svc.list_api_key_rows(id).await.expect("keys");
    assert!(
        listed
            .iter()
            .any(|r| r.label.as_deref() == Some("cli-test")),
        "{listed:?}"
    );

    let revealed = svc.reveal_api_key(id, prov.key_id).await.expect("reveal");
    assert_eq!(revealed, prov.api_key);
}
