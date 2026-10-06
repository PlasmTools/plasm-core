//! Encrypted MCP binding envelopes stored at `plasm:binding:v1:*` keys in AuthStorage.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// JSON `version` field for [`BindingKvV1`].
pub const BINDING_KV_VERSION: u32 = 1;

/// KV key prefix for binding envelopes.
pub const BINDING_KV_PREFIX: &str = "plasm:binding:v1:";

#[derive(Debug, thiserror::Error)]
pub enum ConnectUrlError {
    #[error("URL must not be empty")]
    Empty,
    #[error("invalid URL: {0}")]
    Parse(#[from] url::ParseError),
    #[error("URL must use http or https")]
    UnsupportedScheme,
    #[error("URL must include a host")]
    MissingHost,
}

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
    #[error(
        "binding scope mismatch: expected tenant={} config={} entry={}, got tenant={} config={} entry={}",
        .expected.tenant_id, .expected.mcp_config_id, .expected.entry_id,
        .actual.tenant_id, .actual.mcp_config_id, .actual.entry_id
    )]
    ScopeMismatch {
        expected: Box<BindingScopeV1>,
        actual: Box<BindingScopeV1>,
    },
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
        return Err(BindingKvParseError::ScopeMismatch {
            expected: Box::new(BindingScopeV1 {
                tenant_id: tenant_id.to_owned(),
                mcp_config_id: mcp_config_id.to_owned(),
                entry_id: entry_id.to_owned(),
            }),
            actual: Box::new(env.scope),
        });
    }
    Ok(env)
}

pub fn binding_kv_key_from_uuid(uuid: &str) -> String {
    format!("{BINDING_KV_PREFIX}{uuid}")
}

/// Normalize workspace URL: trim, strip trailing slash, require http(s) scheme.
pub fn normalize_connect_url(raw: &str) -> Result<String, ConnectUrlError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(ConnectUrlError::Empty);
    }
    let parsed = url::Url::parse(s)?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ConnectUrlError::UnsupportedScheme);
    }
    if parsed.host_str().is_none() {
        return Err(ConnectUrlError::MissingHost);
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
        assert_eq!(
            env.values.get("catalog_http_origin").map(String::as_str),
            Some("https://acme.fibery.io")
        );
    }

    #[test]
    fn scoped_binding_parser_preserves_each_mismatched_scope() {
        assert!(std::mem::size_of::<BindingKvParseError>() < 128);
        let scope = BindingScopeV1 {
            tenant_id: "t1".into(),
            mcp_config_id: "c1".into(),
            entry_id: "fibery".into(),
        };
        let raw = serde_json::to_string(&BindingKvV1 {
            version: BINDING_KV_VERSION,
            scope: scope.clone(),
            values: HashMap::new(),
        })
        .unwrap();
        assert_eq!(
            parse_binding_kv_v1_scoped(&raw, "t1", "c1", "fibery")
                .unwrap()
                .scope,
            scope
        );
        for (tenant, config, entry) in [
            ("t2", "c1", "fibery"),
            ("t1", "c2", "fibery"),
            ("t1", "c1", "github"),
        ] {
            let error = parse_binding_kv_v1_scoped(&raw, tenant, config, entry).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "binding scope mismatch: expected tenant={tenant} config={config} entry={entry}, got tenant=t1 config=c1 entry=fibery"
                )
            );
            let BindingKvParseError::ScopeMismatch { expected, actual } = error else {
                panic!("expected a scope mismatch");
            };
            assert_eq!(
                *expected,
                BindingScopeV1 {
                    tenant_id: tenant.into(),
                    mcp_config_id: config.into(),
                    entry_id: entry.into(),
                }
            );
            assert_eq!(*actual, scope);
        }
    }

    #[test]
    fn normalize_connect_url_strips_trailing_slash_path() {
        assert_eq!(
            normalize_connect_url("https://acme.fibery.io/").unwrap(),
            "https://acme.fibery.io"
        );
    }

    #[test]
    fn normalize_connect_url_reports_semantic_failures() {
        assert!(matches!(
            normalize_connect_url("  "),
            Err(ConnectUrlError::Empty)
        ));
        assert!(matches!(
            normalize_connect_url("ftp://acme.fibery.io"),
            Err(ConnectUrlError::UnsupportedScheme)
        ));
        assert!(matches!(
            normalize_connect_url("not a URL"),
            Err(ConnectUrlError::Parse(_))
        ));
    }
}
