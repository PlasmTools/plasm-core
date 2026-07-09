//! SEP-2575 stateless Streamable HTTP MCP (`PLASM_MCP_STATELESS=1`).
//!
//! POST `/mcp` only: per-request `_meta`, `server/discover`, no `initialize` / `MCP-Session-Id`.

mod discover;
mod http;
mod meta;

#[cfg(test)]
mod tests;

pub(crate) use discover::build_discover_result;
pub(crate) use http::router;
pub(crate) use meta::{
    strip_transport_meta_from_params, validate_request_meta, RequestMetaError,
    STATELESS_PROTOCOL_VERSION, SUPPORTED_STATELESS_VERSIONS,
};

/// True when `PLASM_MCP_STATELESS` is `1`, `true`, or `yes` (case-insensitive).
pub fn plasm_mcp_stateless_enabled() -> bool {
    matches!(
        std::env::var("PLASM_MCP_STATELESS")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}
