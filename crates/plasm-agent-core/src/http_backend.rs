//! HTTP origin provenance: catalog YAML placeholders vs REPL/engine overrides vs resolved transport.

use plasm_core::DEFAULT_HTTP_BACKEND;

/// Baked into `domain.yaml` / [`plasm_core::CGS::http_backend`] — may be a host placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogHttpBackend(String);

/// User override from `--backend` / engine `base_url` — concrete workspace origin semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplHttpOverride(String);

/// Post-resolution concrete origin safe for HTTP transport / CML env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHttpOrigin(String);

/// Normalized value for `catalog_http_origin` binding wire (MCP connect or REPL synth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingOriginValue(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplHttpOverrideError {
    Empty,
    /// User pasted catalog placeholder text instead of a real workspace host.
    LiteralCatalogPlaceholder,
}

impl std::fmt::Display for ReplHttpOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "HTTP backend URL must not be empty"),
            Self::LiteralCatalogPlaceholder => write!(
                f,
                "HTTP backend must be a concrete workspace URL, not a catalog placeholder"
            ),
        }
    }
}

impl CatalogHttpBackend {
    /// Sole constructor from CGS / registry load paths.
    pub fn from_cgs_field(raw: &str) -> Self {
        Self(raw.trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Catalog placeholder requiring session bindings or legacy outbound KV before execute.
    pub fn needs_origin_resolution(&self, entry_id: &str) -> bool {
        crate::binding_slots::entry_requires_bindings(entry_id) && self.is_placeholder()
    }

    pub fn is_placeholder(&self) -> bool {
        is_schema_placeholder_http_backend(self.as_str())
    }
}

impl ReplHttpOverride {
    /// After [`crate::backend_normalize::normalize_live_backend_url`].
    pub fn from_cli_normalized(raw: &str) -> Result<Self, ReplHttpOverrideError> {
        let t = raw.trim().trim_end_matches('/');
        if t.is_empty() {
            return Err(ReplHttpOverrideError::Empty);
        }
        if is_fibery_account_placeholder_http_backend(t) || t.contains("YOUR_ACCOUNT") {
            return Err(ReplHttpOverrideError::LiteralCatalogPlaceholder);
        }
        Ok(Self(t.to_string()))
    }

    /// Engine config path (already normalized at bootstrap).
    pub fn from_engine_base(base: &str) -> Result<Self, ReplHttpOverrideError> {
        Self::from_cli_normalized(base)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_binding_origin_value(&self) -> BindingOriginValue {
        BindingOriginValue(self.0.clone())
    }
}

impl ResolvedHttpOrigin {
    pub fn from_catalog(catalog: &CatalogHttpBackend) -> Self {
        Self(catalog.as_str().trim().trim_end_matches('/').to_string())
    }

    pub fn from_engine_override(engine: &ReplHttpOverride) -> Self {
        Self(engine.as_str().to_string())
    }

    pub fn from_resolved_str(raw: &str) -> Self {
        Self(raw.trim().trim_end_matches('/').to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl BindingOriginValue {
    pub fn from_legacy_outbound_kv(raw: &str) -> Self {
        Self(raw.trim().trim_end_matches('/').to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// HTTP origin for plan/live execute: engine harness wins over schema placeholder catalog backends.
pub(crate) fn plan_http_origin(
    engine: Option<&ReplHttpOverride>,
    catalog: Option<&CatalogHttpBackend>,
) -> Option<ResolvedHttpOrigin> {
    match (engine, catalog) {
        (Some(e), Some(c)) if c.is_placeholder() => {
            Some(ResolvedHttpOrigin::from_engine_override(e))
        }
        (_, Some(c)) => Some(ResolvedHttpOrigin::from_catalog(c)),
        (Some(e), None) => Some(ResolvedHttpOrigin::from_engine_override(e)),
        _ => None,
    }
}

fn is_schema_placeholder_http_backend(url: &str) -> bool {
    url == DEFAULT_HTTP_BACKEND
        || url == "http://127.0.0.1:9"
        || url.starts_with("http://127.0.0.1:9/")
        || is_fibery_account_placeholder_http_backend(url)
}

/// Fibery catalogs ship `https://YOUR_ACCOUNT.fibery.io` until connect or host env supplies the workspace host.
fn is_fibery_account_placeholder_http_backend(url: &str) -> bool {
    let t = url.trim().trim_end_matches('/');
    if t.eq_ignore_ascii_case("https://YOUR_ACCOUNT.fibery.io")
        || t.eq_ignore_ascii_case("http://YOUR_ACCOUNT.fibery.io")
    {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| host == "your_account.fibery.io")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_needs_origin_only_for_binding_entries_with_placeholder() {
        let ph = CatalogHttpBackend::from_cgs_field("https://YOUR_ACCOUNT.fibery.io");
        assert!(ph.needs_origin_resolution("fibery"));
        assert!(!ph.needs_origin_resolution("github"));
        let concrete = CatalogHttpBackend::from_cgs_field("https://acme.fibery.io");
        assert!(!concrete.needs_origin_resolution("fibery"));
    }

    #[test]
    fn repl_override_rejects_catalog_placeholder_literals() {
        assert!(ReplHttpOverride::from_cli_normalized("https://acme.fibery.io").is_ok());
        assert!(matches!(
            ReplHttpOverride::from_cli_normalized("https://YOUR_ACCOUNT.fibery.io"),
            Err(ReplHttpOverrideError::LiteralCatalogPlaceholder)
        ));
    }

    #[test]
    fn plan_http_origin_prefers_engine_over_schema_placeholder() {
        let engine = ReplHttpOverride::from_cli_normalized("http://127.0.0.1:8765").unwrap();
        let catalog = CatalogHttpBackend::from_cgs_field("http://127.0.0.1:9");
        assert_eq!(
            plan_http_origin(Some(&engine), Some(&catalog))
                .expect("origin")
                .as_str(),
            "http://127.0.0.1:8765"
        );
        let concrete = CatalogHttpBackend::from_cgs_field("https://api.example.com");
        assert_eq!(
            plan_http_origin(Some(&engine), Some(&concrete))
                .expect("origin")
                .as_str(),
            "https://api.example.com"
        );
    }

    #[test]
    fn fibery_placeholder_backend_detected() {
        assert!(is_fibery_account_placeholder_http_backend(
            "https://YOUR_ACCOUNT.fibery.io"
        ));
        assert!(is_fibery_account_placeholder_http_backend(
            "https://your_account.fibery.io/"
        ));
        assert!(!is_fibery_account_placeholder_http_backend(
            "https://acme.fibery.io"
        ));
    }
}
