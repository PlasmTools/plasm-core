//! Encrypted MCP binding envelopes stored at `plasm:binding:v1:*` keys in AuthStorage.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// JSON `version` field for [`BindingKvV1`].
pub const BINDING_KV_VERSION: u32 = 1;

/// KV key prefix for binding envelopes.
pub const BINDING_KV_PREFIX: &str = "plasm:binding:v1:";

/// Scope triple embedded in every binding envelope (defense in depth).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingScopeV1 {
    pub tenant_id: String,
    pub mcp_config_id: String,
    pub entry_id: String,
}

/// Binding envelope (v1) stored in encrypted KV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingKvV1 {
    pub version: u32,
    pub scope: BindingScopeV1,
    /// Host wire name → resolved value (e.g. `catalog_http_origin`).
    pub values: HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BindingKvParseError {
    #[error("binding credential is empty")]
    Empty,
    #[error("binding credential must be JSON object (BindingKvV1)")]
    NotJsonObject,
    #[error("invalid JSON for binding credential: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported binding credential version: {0} (expected {1})")]
    UnsupportedVersion(u32, u32),
    #[error("binding scope mismatch: {0}")]
    ScopeMismatch(String),
}

pub fn parse_binding_kv_v1(raw: &str) -> Result<BindingKvV1, BindingKvParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BindingKvParseError::Empty);
    }
    let v: serde_json::Value = serde_json::from_str(trimmed)?;
    if !v.is_object() {
        return Err(BindingKvParseError::NotJsonObject);
    }
    let env: BindingKvV1 = serde_json::from_value(v)?;
    if env.version != BINDING_KV_VERSION {
        return Err(BindingKvParseError::UnsupportedVersion(
            env.version,
            BINDING_KV_VERSION,
        ));
    }
    Ok(env)
}

pub fn parse_binding_kv_v1_scoped(
    raw: &str,
    tenant_id: &str,
    mcp_config_id: &str,
    entry_id: &str,
) -> Result<BindingKvV1, BindingKvParseError> {
    let env = parse_binding_kv_v1(raw)?;
    if env.scope.tenant_id != tenant_id
        || env.scope.mcp_config_id != mcp_config_id
        || env.scope.entry_id != entry_id
    {
        return Err(BindingKvParseError::ScopeMismatch(format!(
            "expected tenant={tenant_id} config={mcp_config_id} entry={entry_id}, got tenant={} config={} entry={}",
            env.scope.tenant_id, env.scope.mcp_config_id, env.scope.entry_id
        )));
    }
    Ok(env)
}

pub fn binding_kv_key_from_uuid(uuid: &str) -> String {
    format!("{BINDING_KV_PREFIX}{uuid}")
}

/// Normalize workspace URL: trim, strip trailing slash, require http(s) scheme.
pub fn normalize_connect_url(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("URL must not be empty".into());
    }
    let parsed = url::Url::parse(s).map_err(|e| format!("invalid URL: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("URL must use http or https".into());
    }
    if parsed.host_str().is_none() {
        return Err("URL must include a host".into());
    }
    let mut out = format!("{}://{}", scheme, parsed.host_str().expect("host"));
    if let Some(port) = parsed.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_binding_kv_roundtrip() {
        let raw = r#"{"version":1,"scope":{"tenant_id":"t1","mcp_config_id":"c1","entry_id":"fibery"},"values":{"catalog_http_origin":"https://acme.fibery.io"}}"#;
        let env = parse_binding_kv_v1(raw).expect("parse");
        assert_eq!(env.values.get("catalog_http_origin").map(String::as_str), Some("https://acme.fibery.io"));
    }

    #[test]
    fn normalize_connect_url_strips_trailing_slash_path() {
        assert_eq!(
            normalize_connect_url("https://acme.fibery.io/").unwrap(),
            "https://acme.fibery.io"
        );
    }
}
