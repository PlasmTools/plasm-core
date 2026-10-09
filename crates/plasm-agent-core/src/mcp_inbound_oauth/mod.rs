//! Plasm-owned MCP OAuth authorization, expiring grants and JWT access tokens.

mod client;
mod dcr;
mod error;
mod grants;
mod jwt;
mod metadata_client;
mod pkce;
mod principal;
mod records;
mod refresh_family;
mod resource;
mod session_store;
mod types;

use std::net::IpAddr;
use std::sync::Arc;

use crate::secret_store::SecretStore;
use crate::secret_store::SecretStoreError;

use crate::server_state::PlasmHostState;

pub use error::McpOAuthError;
pub use jwt::VerifiedMcpOAuthAccess;
pub use types::{
    mcp_resource_base_url, AuthorizationRequest, AuthorizeOutcome, McpOAuthRegisterResponse,
    McpOAuthTokenRequest, OAUTH_SCOPE,
};

pub struct McpInboundOAuthService {
    storage: Arc<dyn SecretStore>,
    jwt: jwt::McpOAuthJwt,
    canonical_resource: String,
}

impl McpInboundOAuthService {
    pub(crate) async fn try_from_host_with_resource(
        plasm: &PlasmHostState,
        resource: String,
    ) -> Option<Self> {
        let storage = plasm.auth_storage()?.clone();
        let jwt_secret = plasm
            .incoming_auth
            .as_deref()
            .and_then(|v| v.config().jwt_secret.clone())
            .filter(|s| !s.trim().is_empty())?;
        Self::new_for_resource(storage, jwt_secret, resource).ok()
    }

    #[cfg(test)]
    pub async fn new(
        storage: Arc<dyn SecretStore>,
        jwt_secret: String,
    ) -> Result<Self, SecretStoreError> {
        let canonical_resource = mcp_resource_base_url();
        Self::new_for_resource(storage, jwt_secret, canonical_resource)
    }

    fn new_for_resource(
        storage: Arc<dyn SecretStore>,
        jwt_secret: String,
        canonical_resource: String,
    ) -> Result<Self, SecretStoreError> {
        if jwt_secret.len() < 32 {
            return Err(SecretStoreError::Configuration(
                "JWT signing secret must contain at least 32 bytes",
            ));
        }
        let jwt = jwt::McpOAuthJwt::new(&jwt_secret, &canonical_resource);
        Ok(Self {
            storage,
            jwt,
            canonical_resource,
        })
    }

    pub async fn register_client(
        &self,
        body: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<McpOAuthRegisterResponse, McpOAuthError> {
        dcr::DcrHandle::new(self.storage.as_ref())
            .register_client(body, client_ip)
            .await
    }

    pub fn verify_access_token(
        &self,
        token: &str,
    ) -> Result<VerifiedMcpOAuthAccess, jsonwebtoken::errors::Error> {
        self.jwt.verify_access_token(token)
    }

    pub async fn verify_access_grant(
        &self,
        access: &VerifiedMcpOAuthAccess,
    ) -> Result<(), McpOAuthError> {
        session_store::OAuthSessionStore::new(self.storage.as_ref())
            .validate_access_grant(
                &access.grant_id,
                &access.client_id,
                &access.tenant_id,
                &access.subject,
                &self.canonical_resource,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::{EncryptedSecretStore, MemorySecretStore, SecretEncryption};
    use std::time::Duration;

    #[tokio::test]
    async fn concurrent_grant_redemption_and_refresh_rotation_have_one_winner() {
        let storage: Arc<dyn SecretStore> = Arc::new(EncryptedSecretStore::new(
            MemorySecretStore::new(),
            SecretEncryption::from_key(&[7; 32]).unwrap(),
        ));
        let service = McpInboundOAuthService::new(
            storage.clone(),
            "test-jwt-secret-01234567890123456789012".into(),
        )
        .await
        .unwrap();
        let registered = service.register_client(r#"{"redirect_uris":["https://example.com/callback"],"grant_types":["authorization_code","refresh_token"]}"#, None).await.unwrap();
        let verifier = "valid-verifier-0123456789012345678901234567890123456789";
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let code = records::AuthorizationCode::new(
            registered.client_id.clone(),
            "user".into(),
            "https://example.com/callback".into(),
            vec![OAUTH_SCOPE.into()],
            Some(
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(verifier.as_bytes())),
            ),
            Some("S256".into()),
            Duration::from_secs(600),
        )
        .unwrap();
        let request = McpOAuthTokenRequest {
            grant_type: Some("authorization_code".into()),
            client_id: Some(registered.client_id.clone()),
            code: Some(code.code.clone()),
            redirect_uri: Some(code.redirect_uri.clone()),
            code_verifier: Some(verifier.into()),
            resource: Some(service.canonical_resource.clone()),
            ..Default::default()
        };
        session_store::OAuthSessionStore::new(storage.as_ref())
            .store_auth_code(&session_store::PlasmOAuthAuthCode {
                enhanced: code,
                tenant_id: "tenant".into(),
                resource: service.canonical_resource.clone(),
            })
            .await
            .unwrap();
        let (left, right) = tokio::join!(
            service.exchange_token(&request),
            service.exchange_token(&request)
        );
        let response = match (left, right) {
            (Ok(response), Err(error)) | (Err(error), Ok(response)) => {
                assert_eq!(error.oauth_error_code(), "invalid_grant");
                response
            }
            _ => panic!("exactly one code redemption must succeed"),
        };
        let refresh = McpOAuthTokenRequest {
            grant_type: Some("refresh_token".into()),
            client_id: Some(registered.client_id),
            refresh_token: response.refresh_token,
            resource: Some(service.canonical_resource.clone()),
            ..Default::default()
        };
        let (left, right) = tokio::join!(
            service.exchange_token(&refresh),
            service.exchange_token(&refresh)
        );
        match (left, right) {
            (Ok(response), Err(error)) | (Err(error), Ok(response)) => {
                assert_eq!(error.oauth_error_code(), "invalid_grant");
                let successor = McpOAuthTokenRequest {
                    refresh_token: response.refresh_token,
                    ..refresh.clone()
                };
                assert!(service.exchange_token(&successor).await.is_err());
                let access = service.verify_access_token(&response.access_token).unwrap();
                assert!(service.verify_access_grant(&access).await.is_err());
            }
            _ => panic!("exactly one refresh rotation must succeed"),
        }
    }
}
