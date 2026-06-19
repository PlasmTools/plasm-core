use std::time::{SystemTime, UNIX_EPOCH};

use auth_framework::server::core::common_jwt::{JwtConfig, JwtManager};
use auth_framework::AuthError;
use serde::{Deserialize, Serialize};

use super::error::McpOAuthError;

pub const OAUTH_ACCESS_TOKEN_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthAccessClaims {
    pub iss: String,
    pub sub: String,
    pub aud: Vec<String>,
    pub exp: i64,
    pub iat: i64,
    pub tenant_id: String,
    pub scope: String,
    pub client_id: String,
    pub token_type: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedMcpOAuthAccess {
    pub subject: String,
    pub tenant_id: String,
    pub scope: String,
    pub client_id: String,
}

pub struct McpOAuthJwt {
    manager: JwtManager,
    issuer: String,
}

impl McpOAuthJwt {
    pub fn new(jwt_secret: &str, canonical_resource: &str) -> Self {
        let config = JwtConfig::with_symmetric_key(jwt_secret.as_bytes(), canonical_resource.to_string())
            .with_audience(canonical_resource.to_string())
            .with_expiration(OAUTH_ACCESS_TOKEN_TTL_SECS);
        Self {
            manager: JwtManager::new(config),
            issuer: canonical_resource.to_string(),
        }
    }

    pub fn mint_access_token(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
        resource: &str,
    ) -> Result<String, McpOAuthError> {
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let claims = McpOAuthAccessClaims {
            iss: self.issuer.clone(),
            sub: subject.to_string(),
            aud: vec![resource.to_string()],
            exp: iat + OAUTH_ACCESS_TOKEN_TTL_SECS as i64,
            iat,
            tenant_id: tenant_id.to_string(),
            scope: scope.to_string(),
            client_id: client_id.to_string(),
            token_type: "access_token".to_string(),
        };
        self.manager
            .create_token_with_custom_claims(&claims)
            .map_err(|e| McpOAuthError::server(&e.to_string()))
    }

    pub fn verify_access_token(&self, token: &str) -> Result<VerifiedMcpOAuthAccess, AuthError> {
        let claims: McpOAuthAccessClaims = self.manager.verify_token_with_custom_claims(token)?;
        Ok(VerifiedMcpOAuthAccess {
            subject: claims.sub,
            tenant_id: claims.tenant_id,
            scope: claims.scope,
            client_id: claims.client_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_roundtrip_includes_resource_aud_and_tenant() {
        let jwt = McpOAuthJwt::new(
            "test-jwt-secret-01234567890123456789012",
            "https://platform.plasm.tools/plasm/mcp",
        );
        let resource = "https://platform.plasm.tools/plasm/mcp";
        let token = jwt
            .mint_access_token("client_a", "tenant-a", "user-a", "mcp:tools", resource)
            .expect("mint");
        let verified = jwt.verify_access_token(&token).expect("verify");
        assert_eq!(verified.tenant_id, "tenant-a");
        assert_eq!(verified.subject, "user-a");
        assert_eq!(verified.client_id, "client_a");
        assert_eq!(verified.scope, "mcp:tools");
    }
}
