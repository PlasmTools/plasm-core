//! Typed interpretation of [`AuthInfo`] after Streamable HTTP MCP transport verify.
//!
//! The SDK's [`AuthInfo::client_id`] follows OAuth RFC semantics (dynamic-registration client id).
//! Plasm incoming-auth tenant ids (`gh-*`, org tenant, …) live in [`Self::incoming_tenant_id`] only.

use std::time::SystemTime;

use rust_mcp_sdk::auth::AuthInfo;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::incoming_auth::{IncomingAuthMethod, TenantPrincipal};

pub(crate) const EXTRA_INCOMING_TENANT_ID: &str = "plasm_incoming_tenant_id";
pub(crate) const EXTRA_MCP_CONFIG_ID: &str = "plasm_mcp_config_id";
pub(crate) const EXTRA_MCP_OAUTH: &str = "plasm_mcp_oauth";
pub(crate) const EXTRA_MCP_ANONYMOUS: &str = "plasm_mcp_anonymous";

/// Verified MCP transport identity — canonical source for tenant, subject, and config binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpTransportIdentity {
    pub incoming_tenant_id: String,
    pub subject: String,
    pub method: IncomingAuthMethod,
    pub mcp_config_id: Option<Uuid>,
    /// OAuth dynamic-registration client id (`client_*`); absent for API-key transport.
    pub oauth_client_id: Option<String>,
    pub anonymous: bool,
}

impl McpTransportIdentity {
    pub fn anonymous() -> Self {
        Self {
            incoming_tenant_id: String::new(),
            subject: String::new(),
            method: IncomingAuthMethod::ApiKey,
            mcp_config_id: None,
            oauth_client_id: None,
            anonymous: true,
        }
    }

    pub fn api_key(
        incoming_tenant_id: String,
        subject: String,
        mcp_config_id: Uuid,
    ) -> Self {
        Self {
            incoming_tenant_id,
            subject,
            method: IncomingAuthMethod::ApiKey,
            mcp_config_id: Some(mcp_config_id),
            oauth_client_id: None,
            anonymous: false,
        }
    }

    pub fn oauth(
        incoming_tenant_id: String,
        subject: String,
        oauth_client_id: String,
        mcp_config_id: Uuid,
    ) -> Self {
        Self {
            incoming_tenant_id,
            subject,
            method: IncomingAuthMethod::Jwt,
            mcp_config_id: Some(mcp_config_id),
            oauth_client_id: Some(oauth_client_id),
            anonymous: false,
        }
    }

    pub fn from_auth_info(info: &AuthInfo) -> Option<Self> {
        if info
            .extra
            .as_ref()
            .and_then(|m| m.get(EXTRA_MCP_ANONYMOUS))
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            return Some(Self::anonymous());
        }

        let extra = info.extra.as_ref()?;
        let incoming_tenant_id = extra
            .get(EXTRA_INCOMING_TENANT_ID)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)?;
        let subject = info.user_id.clone().filter(|s| !s.trim().is_empty())?;
        let method = if extra
            .get(EXTRA_MCP_OAUTH)
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            IncomingAuthMethod::Jwt
        } else {
            IncomingAuthMethod::ApiKey
        };
        let mcp_config_id = extra
            .get(EXTRA_MCP_CONFIG_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let oauth_client_id = if method == IncomingAuthMethod::Jwt {
            info.client_id.clone().filter(|s| !s.trim().is_empty())
        } else {
            None
        };
        Some(Self {
            incoming_tenant_id,
            subject,
            method,
            mcp_config_id,
            oauth_client_id,
            anonymous: false,
        })
    }

    pub fn to_tenant_principal(&self) -> Option<TenantPrincipal> {
        if self.anonymous {
            return None;
        }
        Some(TenantPrincipal {
            tenant_id: self.incoming_tenant_id.clone(),
            subject: self.subject.clone(),
            method: self.method.clone(),
        })
    }

    pub fn into_auth_info(
        self,
        token_unique_id: String,
        scopes: Option<Vec<String>>,
        expires_at: Option<SystemTime>,
        mut extra: Map<String, Value>,
    ) -> AuthInfo {
        if self.anonymous {
            extra.insert(EXTRA_MCP_ANONYMOUS.to_string(), json!(true));
            return AuthInfo {
                token_unique_id,
                client_id: None,
                user_id: None,
                scopes,
                expires_at,
                audience: None,
                extra: Some(extra),
            };
        }

        extra.insert(
            EXTRA_INCOMING_TENANT_ID.to_string(),
            json!(self.incoming_tenant_id),
        );
        if let Some(id) = self.mcp_config_id {
            extra.insert(EXTRA_MCP_CONFIG_ID.to_string(), json!(id.to_string()));
        }
        if self.method == IncomingAuthMethod::Jwt {
            extra.insert(EXTRA_MCP_OAUTH.to_string(), json!(true));
        }

        AuthInfo {
            token_unique_id,
            client_id: self.oauth_client_id,
            user_id: Some(self.subject),
            scopes,
            expires_at,
            audience: None,
            extra: Some(extra),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn roundtrip(identity: McpTransportIdentity, extra: Map<String, Value>) -> McpTransportIdentity {
        let info = identity.into_auth_info("tok".into(), None, None, extra);
        McpTransportIdentity::from_auth_info(&info).expect("roundtrip")
    }

    #[test]
    fn oauth_identity_separates_incoming_tenant_from_registration_client_id() {
        let mut extra = Map::new();
        extra.insert("plasm_space_type".into(), json!("personal"));
        let identity = McpTransportIdentity::oauth(
            "gh-85869007".into(),
            "github:85869007".into(),
            "client_d0b3c21a379b4b1990de00f3068908f5".into(),
            Uuid::nil(),
        );
        let parsed = roundtrip(identity, extra);
        assert_eq!(parsed.incoming_tenant_id, "gh-85869007");
        assert_eq!(
            parsed.oauth_client_id.as_deref(),
            Some("client_d0b3c21a379b4b1990de00f3068908f5")
        );
        assert_eq!(parsed.method, IncomingAuthMethod::Jwt);
        let principal = parsed.to_tenant_principal().expect("principal");
        assert_eq!(principal.tenant_id, "gh-85869007");
    }

    #[test]
    fn api_key_identity_uses_incoming_tenant_without_oauth_client_id() {
        let mut extra = Map::new();
        extra.insert("plasm_space_type".into(), json!("personal"));
        let identity = McpTransportIdentity::api_key(
            "gh-10488548".into(),
            "github:10488548".into(),
            Uuid::nil(),
        );
        let parsed = roundtrip(identity, extra);
        assert_eq!(parsed.incoming_tenant_id, "gh-10488548");
        assert!(parsed.oauth_client_id.is_none());
        assert_eq!(parsed.method, IncomingAuthMethod::ApiKey);
    }

    #[test]
    fn anonymous_identity_roundtrips() {
        let parsed = roundtrip(McpTransportIdentity::anonymous(), Map::new());
        assert!(parsed.anonymous);
        assert!(parsed.to_tenant_principal().is_none());
    }
}
