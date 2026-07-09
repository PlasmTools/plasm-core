//! Per-request `_meta` validation (SEP-2575).

use rust_mcp_sdk::schema::{ClientCapabilities, Implementation, InitializeRequestParams, RpcError};
use serde_json::{json, Value};

pub(crate) const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub(crate) const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub(crate) const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

pub(crate) const STATELESS_PROTOCOL_VERSION: &str = "2026-07-28";

pub(crate) const SUPPORTED_STATELESS_VERSIONS: &[&str] =
    &[STATELESS_PROTOCOL_VERSION, "2025-11-25"];

#[derive(Debug)]
pub(crate) enum RequestMetaError {
    InvalidParams(RpcError),
    HeaderMismatch(RpcError),
    UnsupportedVersion {
        #[allow(dead_code)]
        requested: String,
        error: RpcError,
    },
}

impl RequestMetaError {
    pub(crate) fn into_rpc_error(self) -> RpcError {
        match self {
            Self::InvalidParams(e)
            | Self::HeaderMismatch(e)
            | Self::UnsupportedVersion { error: e, .. } => e,
        }
    }

    pub(crate) fn http_status(&self) -> http::StatusCode {
        match self {
            Self::InvalidParams(_) | Self::HeaderMismatch(_) | Self::UnsupportedVersion { .. } => {
                http::StatusCode::BAD_REQUEST
            }
        }
    }
}

/// Validate `_meta` on request `params` and optional `MCP-Protocol-Version` header.
pub(crate) fn validate_request_meta(
    params: Option<&Value>,
    header_protocol_version: Option<&str>,
) -> Result<InitializeRequestParams, RequestMetaError> {
    let meta = params
        .and_then(|p| p.get("_meta"))
        .ok_or_else(|| invalid_params("missing required _meta on request params"))?;

    let protocol_version = meta
        .get(META_PROTOCOL_VERSION)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            invalid_params(&format!(
                "missing required _meta field `{META_PROTOCOL_VERSION}`"
            ))
        })?
        .to_string();

    if let Some(header_version) = header_protocol_version
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if header_version != protocol_version {
            return Err(RequestMetaError::HeaderMismatch(RpcError {
                code: -32020,
                message:
                    "Header mismatch: MCP-Protocol-Version does not match _meta protocol version"
                        .to_string(),
                data: Some(json!({
                    "header": header_version,
                    "meta": protocol_version,
                })),
            }));
        }
    }

    if !SUPPORTED_STATELESS_VERSIONS.contains(&protocol_version.as_str()) {
        return Err(RequestMetaError::UnsupportedVersion {
            requested: protocol_version.clone(),
            error: RpcError {
                code: -32019,
                message: "Unsupported protocol version".to_string(),
                data: Some(json!({
                    "supported": SUPPORTED_STATELESS_VERSIONS,
                    "requested": protocol_version,
                })),
            },
        });
    }

    let client_info_value = meta.get(META_CLIENT_INFO).ok_or_else(|| {
        invalid_params(&format!(
            "missing required _meta field `{META_CLIENT_INFO}`"
        ))
    })?;
    let client_info: Implementation = serde_json::from_value(client_info_value.clone())
        .map_err(|_| invalid_params(&format!("invalid _meta field `{META_CLIENT_INFO}`")))?;

    let client_caps_value = meta.get(META_CLIENT_CAPABILITIES).ok_or_else(|| {
        invalid_params(&format!(
            "missing required _meta field `{META_CLIENT_CAPABILITIES}`"
        ))
    })?;
    let capabilities: ClientCapabilities = serde_json::from_value(client_caps_value.clone())
        .map_err(|_| {
            invalid_params(&format!("invalid _meta field `{META_CLIENT_CAPABILITIES}`"))
        })?;

    Ok(InitializeRequestParams {
        protocol_version,
        capabilities,
        client_info,
        meta: None,
    })
}

/// Remove transport envelope keys from `params._meta` before SDK deserialization.
pub(crate) fn strip_transport_meta_from_params(params: &mut Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    let Some(meta) = obj.get_mut("_meta").and_then(Value::as_object_mut) else {
        obj.remove("_meta");
        return;
    };
    meta.remove(META_PROTOCOL_VERSION);
    meta.remove(META_CLIENT_INFO);
    meta.remove(META_CLIENT_CAPABILITIES);
    if meta.is_empty() {
        obj.remove("_meta");
    }
}

fn invalid_params(message: &str) -> RequestMetaError {
    RequestMetaError::InvalidParams(RpcError::invalid_params().with_message(message))
}
