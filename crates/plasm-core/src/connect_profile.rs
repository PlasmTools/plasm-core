//! Typed connect-eligibility projection derived from CGS `auth:` / `oauth:` metadata.
//!
//! This is the canonical classification used by tool-model HTTP payloads so UIs do not
//! re-infer behavior from raw [`crate::schema::AuthScheme`] variants alone.

use crate::schema::{AuthScheme, OauthExtension};
use serde::{Deserialize, Serialize};

/// High-level auth surface for outbound connect UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogAuthCapability {
    /// Only `none` — public / no outbound secret.
    Public,
    /// `api_key` only.
    ApiKeyOnly,
    /// `oauth2` only (OAuth extension and/or client-credentials auth scheme).
    OauthOnly,
    /// Both `api_key` and `oauth2` are valid for this catalog.
    ApiKeyAndOauth,
}

/// OAuth-specific metadata for connect flows (e.g. authorization code with an external provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogOauthCapability {
    /// `oauth.provider` is present and non-empty when an `oauth:` block exists.
    pub provider_present: bool,
    /// Non-empty `scopes` and/or `default_scope_sets` in the OAuth extension.
    pub scope_catalog_present: bool,
}

/// Stable JSON projection for tool-model consumers and policy resolvers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogConnectProfile {
    pub capability: CatalogAuthCapability,
    pub oauth: CatalogOauthCapability,
    /// Public/no-credentials mode (CGS `none` affordance).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_public_mode: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_api_key: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_oauth: bool,
}

fn normalized_auth_kind_tags(
    auth: Option<&AuthScheme>,
    oauth: Option<&OauthExtension>,
) -> Vec<String> {
    if matches!(auth, Some(AuthScheme::None)) {
        return vec!["none".to_string()];
    }
    let mut out = Vec::new();
    if oauth.is_some() {
        out.push("oauth2".to_string());
    }
    match auth {
        Some(AuthScheme::ApiKeyHeader { .. }) | Some(AuthScheme::ApiKeyQuery { .. }) => {
            if !out.iter().any(|k| k == "api_key") {
                out.push("api_key".to_string());
            }
        }
        // Env-/KV-held bearer secrets (PATs, Cloudflare API tokens, etc.) are API-key style for
        // outbound connect UX — distinct from OAuth client-credentials machine flow below.
        Some(AuthScheme::BearerToken { .. }) => {
            if !out.iter().any(|k| k == "api_key") {
                out.push("api_key".to_string());
            }
        }
        Some(AuthScheme::Oauth2ClientCredentials { .. }) => {
            if !out.iter().any(|k| k == "oauth2") {
                out.push("oauth2".to_string());
            }
        }
        Some(AuthScheme::None) | None => {}
    }
    if out.is_empty() {
        out.push("none".to_string());
    }
    out
}

/// Build the typed profile from CGS `auth` / `oauth` blocks.
pub fn catalog_connect_profile(
    auth: Option<&AuthScheme>,
    oauth: Option<&OauthExtension>,
) -> CatalogConnectProfile {
    let kinds = normalized_auth_kind_tags(auth, oauth);
    let has_public_mode = kinds.iter().any(|k| k == "none");
    let has_api_key = kinds.iter().any(|k| k == "api_key");
    let has_oauth = kinds.iter().any(|k| k == "oauth2");

    let capability = if has_public_mode && !has_api_key && !has_oauth {
        CatalogAuthCapability::Public
    } else if has_api_key && has_oauth {
        CatalogAuthCapability::ApiKeyAndOauth
    } else if has_api_key {
        CatalogAuthCapability::ApiKeyOnly
    } else if has_oauth {
        CatalogAuthCapability::OauthOnly
    } else {
        CatalogAuthCapability::Public
    };

    let oauth_cap = match oauth {
        Some(o) => CatalogOauthCapability {
            provider_present: !o.provider.trim().is_empty(),
            scope_catalog_present: !o.scopes.is_empty() || !o.default_scope_sets.is_empty(),
        },
        None => CatalogOauthCapability {
            provider_present: false,
            scope_catalog_present: false,
        },
    };

    CatalogConnectProfile {
        capability,
        oauth: oauth_cap,
        has_public_mode,
        has_api_key,
        has_oauth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{AuthScheme, OauthExtension};

    #[test]
    fn explicit_none_is_public_profile() {
        let auth = AuthScheme::None;
        let p = catalog_connect_profile(Some(&auth), None);
        assert_eq!(p.capability, CatalogAuthCapability::Public);
        assert!(p.has_public_mode);
        assert!(!p.has_api_key);
        assert!(!p.has_oauth);
    }

    #[test]
    fn api_key_header_yields_api_key_only() {
        let auth = AuthScheme::ApiKeyHeader {
            header: "X-Api-Key".into(),
            env: Some("K".into()),
            hosted_kv: None,
        };
        let p = catalog_connect_profile(Some(&auth), None);
        assert_eq!(p.capability, CatalogAuthCapability::ApiKeyOnly);
        assert!(!p.has_public_mode);
        assert!(p.has_api_key);
        assert!(!p.has_oauth);
    }

    #[test]
    fn oauth_extension_plus_api_key_is_mixed() {
        let auth = AuthScheme::ApiKeyHeader {
            header: "X-Api-Key".into(),
            env: Some("K".into()),
            hosted_kv: None,
        };
        let oauth = OauthExtension {
            provider: "google".into(),
            scopes: Default::default(),
            default_scope_sets: Default::default(),
            requirements: Default::default(),
        };
        let p = catalog_connect_profile(Some(&auth), Some(&oauth));
        assert_eq!(p.capability, CatalogAuthCapability::ApiKeyAndOauth);
        assert!(p.has_api_key);
        assert!(p.has_oauth);
        assert!(p.oauth.provider_present);
    }

    #[test]
    fn bearer_token_env_is_api_key_only() {
        let auth = AuthScheme::BearerToken {
            env: Some("CLOUDFLARE_API_TOKEN".into()),
            hosted_kv: None,
            optional_env: false,
        };
        let p = catalog_connect_profile(Some(&auth), None);
        assert_eq!(p.capability, CatalogAuthCapability::ApiKeyOnly);
        assert!(p.has_api_key);
        assert!(!p.has_oauth);
    }

    #[test]
    fn bearer_token_plus_oauth_extension_is_mixed() {
        let auth = AuthScheme::BearerToken {
            env: Some("GITHUB_TOKEN".into()),
            hosted_kv: None,
            optional_env: false,
        };
        let oauth = OauthExtension {
            provider: "github".into(),
            scopes: Default::default(),
            default_scope_sets: Default::default(),
            requirements: Default::default(),
        };
        let p = catalog_connect_profile(Some(&auth), Some(&oauth));
        assert_eq!(p.capability, CatalogAuthCapability::ApiKeyAndOauth);
        assert!(p.has_api_key);
        assert!(p.has_oauth);
    }

    #[test]
    fn oauth2_client_credentials_is_oauth_only() {
        let auth = AuthScheme::Oauth2ClientCredentials {
            token_url: "https://example.com/token".into(),
            client_id_env: Some("CID".into()),
            client_id_hosted_kv: None,
            client_secret_env: Some("SEC".into()),
            client_secret_hosted_kv: None,
            scopes: vec![],
        };
        let p = catalog_connect_profile(Some(&auth), None);
        assert_eq!(p.capability, CatalogAuthCapability::OauthOnly);
        assert!(!p.has_api_key);
        assert!(p.has_oauth);
    }
}
