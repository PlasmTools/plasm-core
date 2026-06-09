//! Clipboard, storage paths, and MCP client config helpers

use super::*;

pub(crate) const MCP_JSON_PLACEHOLDER_BEARER: &str = "Bearer <api_key>";
pub(crate) const PLASM_CLI_PLACEHOLDER_API_KEY: &str = "<api_key>";

pub(crate) fn bearer_authorization_value(raw_secret: Option<&str>) -> String {
    match raw_secret {
        None => MCP_JSON_PLACEHOLDER_BEARER.to_string(),
        Some(raw) => {
            let t = raw.trim();
            if t.is_empty() {
                MCP_JSON_PLACEHOLDER_BEARER.to_string()
            } else if t.len() >= 7 && t[..7].eq_ignore_ascii_case("bearer ") {
                t.to_string()
            } else {
                format!("Bearer {t}")
            }
        }
    }
}

pub(crate) fn mcp_client_json_config(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> Result<String, String> {
    let auth = bearer_authorization_value(raw_secret);
    let value = serde_json::json!({
        "mcpServers": {
            "plasm": {
                "type": "streamableHttp",
                "url": listen.client_mcp_streamable_url(),
                "headers": {
                    "Authorization": auth
                }
            }
        }
    });
    serde_json::to_string_pretty(&value)
        .map(|s| format!("{s}\n"))
        .map_err(|e| e.to_string())
}

pub(crate) fn plasm_cli_api_key_value(raw_secret: Option<&str>) -> String {
    match raw_secret {
        None => PLASM_CLI_PLACEHOLDER_API_KEY.to_string(),
        Some(raw) => {
            let t = raw.trim();
            if t.is_empty() {
                PLASM_CLI_PLACEHOLDER_API_KEY.to_string()
            } else {
                t.to_string()
            }
        }
    }
}

pub(crate) fn plasm_cli_profile_json_config(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> Result<String, String> {
    let value = serde_json::json!({
        "server": listen.client_http_origin(),
        "api_key": plasm_cli_api_key_value(raw_secret),
    });
    serde_json::to_string_pretty(&value)
        .map(|s| format!("{s}\n"))
        .map_err(|e| e.to_string())
}

pub(crate) fn plasm_cli_init_command_line(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> String {
    format!(
        "plasm init --server {} --api-key {}",
        listen.client_http_origin(),
        plasm_cli_api_key_value(raw_secret)
    )
}

pub(crate) fn push_json_block_lines(lines: &mut Vec<Line<'static>>, json: &str) {
    for line in json.lines() {
        lines.push(Line::from(Span::styled(line.to_string(), dim_style())));
    }
}
pub(crate) fn api_key_row_label(k: &McpConfigApiKeyRow) -> String {
    match k.label.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => format!("(unnamed · fp:{})", fingerprint_head(&k.fingerprint)),
    }
}

pub(crate) fn fingerprint_head(fingerprint: &str) -> &str {
    let trimmed = fingerprint.trim();
    if trimmed.is_empty() {
        "unknown"
    } else {
        &trimmed[..trimmed.len().min(8)]
    }
}

pub(crate) fn api_key_row_copy_line(k: &McpConfigApiKeyRow) -> String {
    api_key_row_label(k)
}

pub(crate) fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}

pub(crate) fn env_nonempty_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn storage_backend_summary(
    embedded_autostart: bool,
    skip_reason: Option<&str>,
) -> (&'static str, String) {
    if embedded_autostart {
        (
            "Embedded Postgres",
            "This appliance is managing its own local PostgreSQL 15 cluster.".into(),
        )
    } else {
        (
            "External / disabled Postgres",
            skip_reason
                .unwrap_or("Embedded Postgres is not active for this appliance.")
                .to_string(),
        )
    }
}

pub(crate) fn storage_postgres_data_dir() -> String {
    env_nonempty_string("PLASM_EMBEDDED_POSTGRES_DATA_DIR")
        .or_else(|| env_nonempty_string("PGDATA"))
        .unwrap_or_else(|| "managed OS cache (use --data-dir to pin it)".into())
}

pub(crate) fn storage_local_state_dir() -> String {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "unavailable (HOME / PLASM_LOCAL_STATE_DIR unset)".into())
}

pub(crate) fn storage_auth_key_path() -> String {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|p| {
            p.join("bootstrap-secrets")
                .join("AUTH_STORAGE_ENCRYPTION_KEY")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "unavailable until local state root is known".into())
}
