//! Host-owned binding slot registry and connect requirements (not catalog-author-defined).

use indexmap::IndexMap;
use plasm_runtime::binding_kv::normalize_connect_url;
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

/// Host-controlled slots — plugin authors cannot extend at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingSlot {
    /// Substitutes placeholder catalog `http_backend` (Fibery workspace origin).
    CatalogHttpOrigin,
}

impl BindingSlot {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::CatalogHttpOrigin => plasm_core::bind_wire_validate::HOST_BINDING_WIRES[0],
        }
    }

    pub fn from_wire_name(name: &str) -> Option<Self> {
        if plasm_core::bind_wire_validate::is_known_host_binding_wire(name) {
            Some(Self::CatalogHttpOrigin)
        } else {
            None
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::CatalogHttpOrigin]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectFieldKind {
    Url,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretConnectKindJson {
    ApiKeyHeader,
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SecretConnectKind {
    ApiKeyHeader {
        header: &'static str,
        token_prefix: &'static str,
    },
    None,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BindingConnectSpecJson {
    pub slot: &'static str,
    pub wire: &'static str,
    pub label: &'static str,
    pub kind: ConnectFieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<&'static str>,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SecretConnectSpecJson {
    pub kind: SecretConnectKindJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_prefix: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConnectRequirementsJson {
    pub entry_id: &'static str,
    pub secret: SecretConnectSpecJson,
    pub bindings: &'static [BindingConnectSpecJson],
}

#[derive(Debug, Clone, Copy)]
pub struct BindingConnectSpec {
    pub slot: BindingSlot,
    pub kind: ConnectFieldKind,
    pub label: &'static str,
    pub example: Option<&'static str>,
    pub required: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SecretConnectSpec {
    pub kind: SecretConnectKind,
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectRequirements {
    pub entry_id: &'static str,
    pub secret: SecretConnectSpec,
    pub bindings: &'static [BindingConnectSpec],
}

const FIBERY_BINDINGS: &[BindingConnectSpec] = &[BindingConnectSpec {
    slot: BindingSlot::CatalogHttpOrigin,
    kind: ConnectFieldKind::Url,
    label: "Workspace URL",
    example: Some("https://your-account.fibery.io"),
    required: true,
}];

const FIBERY_CONNECT: ConnectRequirements = ConnectRequirements {
    entry_id: "fibery",
    secret: SecretConnectSpec {
        kind: SecretConnectKind::ApiKeyHeader {
            header: "Authorization",
            token_prefix: "Token ",
        },
    },
    bindings: FIBERY_BINDINGS,
};

const CONNECT_REGISTRY: &[ConnectRequirements] = &[FIBERY_CONNECT];

pub fn connect_requirements_for_entry(entry_id: &str) -> Option<&'static ConnectRequirements> {
    CONNECT_REGISTRY.iter().find(|r| r.entry_id == entry_id)
}

pub fn entry_requires_bindings(entry_id: &str) -> bool {
    connect_requirements_for_entry(entry_id)
        .map(|r| !r.bindings.is_empty())
        .unwrap_or(false)
}

pub fn connect_requirements_json(entry_id: &str) -> Option<ConnectRequirementsJsonOwned> {
    let req = connect_requirements_for_entry(entry_id)?;
    Some(ConnectRequirementsJsonOwned {
        entry_id: req.entry_id.to_string(),
        secret: secret_connect_spec_json(req.secret),
        bindings: binding_specs_json(req.entry_id, req.bindings),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectRequirementsJsonOwned {
    pub entry_id: String,
    pub secret: SecretConnectSpecJson,
    pub bindings: Vec<BindingConnectSpecJson>,
}

fn secret_connect_spec_json(spec: SecretConnectSpec) -> SecretConnectSpecJson {
    match spec.kind {
        SecretConnectKind::ApiKeyHeader {
            header,
            token_prefix,
        } => SecretConnectSpecJson {
            kind: SecretConnectKindJson::ApiKeyHeader,
            header: Some(header),
            token_prefix: Some(token_prefix),
        },
        SecretConnectKind::None => SecretConnectSpecJson {
            kind: SecretConnectKindJson::None,
            header: None,
            token_prefix: None,
        },
    }
}

fn binding_specs_json(
    _entry_id: &str,
    specs: &[BindingConnectSpec],
) -> Vec<BindingConnectSpecJson> {
    specs
        .iter()
        .map(|spec| BindingConnectSpecJson {
            slot: spec.slot.wire_name(),
            wire: spec.slot.wire_name(),
            label: spec.label,
            kind: spec.kind,
            example: spec.example,
            required: spec.required,
        })
        .collect()
}

/// Scope triple for binding KV reads/writes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BindingScope {
    pub tenant_id: String,
    pub mcp_config_id: Uuid,
    pub entry_id: String,
}

impl BindingScope {
    pub fn new(
        tenant_id: impl Into<String>,
        mcp_config_id: Uuid,
        entry_id: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            mcp_config_id,
            entry_id: entry_id.into(),
        }
    }
}

/// Session-constant binding values keyed by host wire name.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionBindingMap {
    pub scope: Option<BindingScope>,
    pub values: IndexMap<String, String>,
}

impl SessionBindingMap {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_values(scope: BindingScope, values: IndexMap<String, String>) -> Self {
        Self {
            scope: Some(scope),
            values,
        }
    }

    pub fn get_wire(&self, wire: &str) -> Option<&str> {
        self.values.get(wire).map(String::as_str)
    }

    /// Minijinja / overlay context object: `{ "catalog_http_origin": "https://…" }`.
    pub fn minijinja_bind_object(&self) -> IndexMap<String, String> {
        self.values.clone()
    }

    /// CML env entries prefixed as `bind_<wire>` for flat env merge.
    pub fn cml_env_entries(&self) -> IndexMap<String, String> {
        self.values
            .iter()
            .map(|(k, v)| (format!("bind_{k}"), v.clone()))
            .collect()
    }
}

/// Validate and normalize connect form values against host spec.
pub fn normalize_connect_binding_values(
    entry_id: &str,
    raw: &HashMap<String, String>,
) -> Result<IndexMap<String, String>, String> {
    let Some(req) = connect_requirements_for_entry(entry_id) else {
        if raw.is_empty() {
            return Ok(IndexMap::new());
        }
        return Err(format!(
            "catalog `{entry_id}` does not accept binding values"
        ));
    };
    let mut out = IndexMap::new();
    for spec in req.bindings {
        let wire = spec.slot.wire_name();
        let val = raw
            .get(wire)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{} is required", spec.label))?;
        let normalized = match spec.kind {
            ConnectFieldKind::Url => normalize_connect_url(val)?,
            ConnectFieldKind::Text => val.to_string(),
        };
        out.insert(wire.to_string(), normalized);
    }
    Ok(out)
}

/// Whether all required binding slots are present in `values`.
pub fn bindings_complete_for_entry(entry_id: &str, values: &IndexMap<String, String>) -> bool {
    let Some(req) = connect_requirements_for_entry(entry_id) else {
        return true;
    };
    req.bindings.iter().all(|spec| {
        if !spec.required {
            return true;
        }
        values
            .get(spec.slot.wire_name())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    })
}

/// Resolve effective HTTP origin from catalog backend + session bindings (+ legacy outbound KV).
pub fn resolve_catalog_http_backend(
    entry_id: &str,
    catalog_backend: &crate::http_backend::CatalogHttpBackend,
    bindings: Option<&SessionBindingMap>,
    legacy_outbound_http_backend: Option<&crate::http_backend::BindingOriginValue>,
) -> Result<crate::http_backend::ResolvedHttpOrigin, String> {
    if !catalog_backend.needs_origin_resolution(entry_id) {
        return Ok(crate::http_backend::ResolvedHttpOrigin::from_catalog(
            catalog_backend,
        ));
    }
    if let Some(map) = bindings {
        if let Some(origin) = map.get_wire(BindingSlot::CatalogHttpOrigin.wire_name()) {
            return Ok(crate::http_backend::ResolvedHttpOrigin::from_resolved_str(
                origin,
            ));
        }
    }
    if let Some(legacy) = legacy_outbound_http_backend {
        let trimmed = legacy.as_str();
        if !trimmed.is_empty() {
            return Ok(crate::http_backend::ResolvedHttpOrigin::from_resolved_str(
                trimmed,
            ));
        }
    }
    Err(format!(
        "Workspace URL not configured — connect {entry_id} in MCP settings"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibery_connect_requirements_present() {
        let req = connect_requirements_for_entry("fibery").expect("fibery");
        assert_eq!(req.bindings.len(), 1);
    }

    #[test]
    fn resolve_origin_from_bindings() {
        let mut values = IndexMap::new();
        values.insert(
            "catalog_http_origin".into(),
            "https://acme.fibery.io".into(),
        );
        let map =
            SessionBindingMap::from_values(BindingScope::new("t1", Uuid::nil(), "fibery"), values);
        let origin = resolve_catalog_http_backend(
            "fibery",
            &crate::http_backend::CatalogHttpBackend::from_cgs_field(
                "https://YOUR_ACCOUNT.fibery.io",
            ),
            Some(&map),
            None,
        )
        .expect("resolve");
        assert_eq!(origin.as_str(), "https://acme.fibery.io");
    }
}
