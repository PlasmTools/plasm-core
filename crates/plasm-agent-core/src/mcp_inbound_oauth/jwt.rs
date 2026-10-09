use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
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
    pub grant_id: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedMcpOAuthAccess {
    pub subject: String,
    pub tenant_id: String,
    pub scope: String,
    pub client_id: String,
    pub grant_id: String,
}

pub struct McpOAuthJwt {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    issuer: String,
}

impl McpOAuthJwt {
    pub fn new(jwt_secret: &str, canonical_resource: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(jwt_secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(jwt_secret.as_bytes()),
            issuer: canonical_resource.to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mint_access_token(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
        resource: &str,
        grant_id: &str,
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
            grant_id: grant_id.into(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .map_err(McpOAuthError::from)
    }

    pub fn verify_access_token(
        &self,
        token: &str,
    ) -> Result<VerifiedMcpOAuthAccess, jsonwebtoken::errors::Error> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims =
            jsonwebtoken::decode::<McpOAuthAccessClaims>(token, &self.decoding_key, &validation)?
                .claims;
        if claims.token_type != "access_token"
            || claims.scope != super::types::OAUTH_SCOPE
            || claims.tenant_id.is_empty()
            || claims.sub.is_empty()
            || claims.grant_id.is_empty()
        {
            return Err(jsonwebtoken::errors::ErrorKind::InvalidToken.into());
        }
        Ok(VerifiedMcpOAuthAccess {
            subject: claims.sub,
            tenant_id: claims.tenant_id,
            scope: claims.scope,
            client_id: claims.client_id,
            grant_id: claims.grant_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_rejects_wrong_resource_issuer_and_token_kind() {
        let resource = "https://example.com/mcp";
        let secret = "test-jwt-secret-01234567890123456789012";
        let jwt = McpOAuthJwt::new(secret, resource);
        let token = jwt
            .mint_access_token("client", "tenant", "user", "mcp:tools", resource, "grant")
            .unwrap();
        let claims = jsonwebtoken::decode::<McpOAuthAccessClaims>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &{
                let mut validation = Validation::new(Algorithm::HS256);
                validation.set_audience(&[resource]);
                validation
            },
        )
        .unwrap()
        .claims;
        for field in ["aud", "iss", "token_type", "scope"] {
            let mut altered = claims.clone();
            match field {
                "aud" => altered.aud = vec!["https://other.example/mcp".into()],
                "iss" => altered.iss = "https://other.example/mcp".into(),
                "token_type" => altered.token_type = "refresh_token".into(),
                _ => altered.scope = "unsupported".into(),
            }
            let forged = jsonwebtoken::encode(
                &Header::new(Algorithm::HS256),
                &altered,
                &EncodingKey::from_secret(secret.as_bytes()),
            )
            .unwrap();
            assert!(
                jwt.verify_access_token(&forged).is_err(),
                "accepted invalid {field}"
            );
        }
    }

    #[test]
    fn jwt_roundtrip_includes_resource_aud_and_tenant() {
        let jwt = McpOAuthJwt::new(
            "test-jwt-secret-01234567890123456789012",
            "https://platform.plasm.tools/plasm/mcp",
        );
        let resource = "https://platform.plasm.tools/plasm/mcp";
        let token = jwt
            .mint_access_token(
                "client_a",
                "tenant-a",
                "user-a",
                "mcp:tools",
                resource,
                "grant",
            )
            .expect("mint");
        let verified = jwt.verify_access_token(&token).expect("verify");
        assert_eq!(verified.tenant_id, "tenant-a");
        assert_eq!(verified.subject, "user-a");
        assert_eq!(verified.client_id, "client_a");
        assert_eq!(verified.scope, "mcp:tools");
    }
}
