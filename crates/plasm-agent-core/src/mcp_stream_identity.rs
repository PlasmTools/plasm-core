//! Verified request identity; OAuth client IDs and incoming tenant IDs remain distinct.

use uuid::Uuid;

use crate::incoming_auth::{IncomingAuthMethod, TenantPrincipal};

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

    pub fn api_key(incoming_tenant_id: String, subject: String, mcp_config_id: Uuid) -> Self {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_identity_separates_incoming_tenant_from_registration_client_id() {
        let identity = McpTransportIdentity::oauth(
            "gh-85869007".into(),
            "github:85869007".into(),
            "client_d0b3c21a379b4b1990de00f3068908f5".into(),
            Uuid::nil(),
        );
        let parsed = identity;
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
        let identity = McpTransportIdentity::api_key(
            "gh-10488548".into(),
            "github:10488548".into(),
            Uuid::nil(),
        );
        let parsed = identity;
        assert_eq!(parsed.incoming_tenant_id, "gh-10488548");
        assert!(parsed.oauth_client_id.is_none());
        assert_eq!(parsed.method, IncomingAuthMethod::ApiKey);
    }

    #[test]
    fn anonymous_identity_has_no_principal() {
        let parsed = McpTransportIdentity::anonymous();
        assert!(parsed.anonymous);
        assert!(parsed.to_tenant_principal().is_none());
    }
}
